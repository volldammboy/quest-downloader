//! Descarga PCVR (Rift) vía manifiesto + segmentos (protocolo del cliente PC).
//!
//! El ZIP directo 404 con tokens; el manifiesto exige sesión de navegador
//! (cookies). Flujo: manifiesto (ZIP con manifest.json) → segmentos
//! (deflate) → ficheros en destino.

use std::io::{Cursor, Read, Write};
use std::time::Duration;

fn u64f(v: Option<&serde_json::Value>) -> u64 {
    v.and_then(|x| {
        x.as_u64()
            .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
    })
    .unwrap_or(0)
}

/// Descarga el build Rift a `dest_dir`. `cb(recibidos, total)` = progreso.
pub fn download_rift(
    dest_dir: &std::path::Path,
    binary_id: &str,
    cookie: &str,
    cb: &dyn Fn(u64, u64),
) -> Result<(), String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) QD/1.0")
        .build()
        .map_err(|e| e.to_string())?;
    let mani_url = format!(
        "https://securecdn.oculus.com/binaries/download/?id={binary_id}&get_manifest=1"
    );
    let resp = http
        .get(&mani_url)
        .header("Cookie", cookie)
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().map_err(|e| e.to_string())?.to_vec();
    if status != 200 || bytes.len() < 4 || &bytes[0..2] != b"PK" {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(160)]).to_string();
        crate::meta_auth::note("rift", &format!("manifiesto HTTP {status}: {head}"));
        return Err(format!("Manifiesto Rift: HTTP {status}. Reenlaza la sesión del navegador."));
    }
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut manifest = String::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        if f.name().ends_with(".json") {
            f.read_to_string(&mut manifest)
                .map_err(|e| e.to_string())?;
            break;
        }
    }
    if manifest.is_empty() {
        return Err("Manifiesto Rift vacío.".into());
    }
    let v: serde_json::Value =
        serde_json::from_str(&manifest).map_err(|e| e.to_string())?;
    let files = v
        .get("files")
        .and_then(|x| x.as_object())
        .ok_or_else(|| {
            let keys: Vec<String> = v
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            crate::meta_auth::note("rift", &format!("manifiesto sin files: {}", keys.join(",")));
            "Manifiesto Rift con forma desconocida.".to_string()
        })?;
    let total: u64 = files.values().map(|f| u64f(f.get("size"))).sum();
    let mut done = 0u64;
    cb(0, total);
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    for (rel, f) in files {
        if rel.contains("..") {
            continue;
        }
        let dest = dest_dir.join(&rel.replace('/', &std::path::MAIN_SEPARATOR.to_string()));
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
        // Forma real (manifiestos locales del cliente PC):
        // segments: [[offset, sha256hex, tam_comp], ...], segmentSize 10MB.
        let segs: Vec<String> = f
            .get("segments")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|s| {
                s.as_array().and_then(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .find(|x| x.len() == 64)
                        .map(|x| x.to_string())
                })
                .or_else(|| {
                    if let Some(a) = s.as_str() {
                        if a.len() == 64 {
                            return Some(a.to_string());
                        }
                    }
                    None
                })
            })
            .collect();
        for sha in segs {
            let url = format!(
                "https://securecdn.oculus.com/binaries/segment/?binary_id={binary_id}&segment_sha256={sha}"
            );
            let resp = http
                .get(&url)
                .header("Cookie", cookie)
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("Segmento Rift: HTTP {}.", resp.status()));
            }
            let body = resp.bytes().map_err(|e| e.to_string())?.to_vec();
            if body.len() < 2 {
                return Err("Segmento Rift vacío.".into());
            }
            let mut dec = flate2::read::DeflateDecoder::new(&body[2..]);
            let mut buf = Vec::new();
            dec.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            out.write_all(&buf).map_err(|e| e.to_string())?;
            done += buf.len() as u64;
            cb(done, total);
        }
    }
    Ok(())
}
