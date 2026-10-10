//! Vinculación Meta como usuario normal (flujo SSO nativo).
//!
//! Abre Meta en una ventana desechable de Edge: inicias sesión allí (tu
//! contraseña solo la ve Meta) y Meta devuelve el token de tu cuenta, que se
//! guarda cifrado con DPAPI. Con ese token se lee tu biblioteca real.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const FRL_APP: &str = "512466987071624";
const CLIENT: &str = "FRL|512466987071624|01d4a1f7fd0682aea7ee8ae987704d63";
const META: &str = "https://meta.graph.meta.com";
const GRAPH: &str = "https://graph.oculus.com/graphql";
const LIBRARY_DOC: &str = "4850747515044496";
const PROFILE_DOC: &str = "24112177345042346";
const PROFILE_APP: &str = "1582076955407037";
pub const DELIVERY_APP: &str = "1481000308606657";
const QUEST: &[&str] = &[
    "QUEST",
    "MONTEREY",
    "HOLLYWOOD",
    "SEACLIFF",
    "EUREKA",
    "PANTHER",
];

fn store_path() -> std::path::PathBuf {
    crate::store::app_dir().join("meta.bin")
}

fn log_path() -> std::path::PathBuf {
    crate::store::app_dir().join("meta.log")
}

fn exe_log_path() -> std::path::PathBuf {
    crate::store::base_dir().join("qd.log")
}

pub fn log(step: &str, status: &str, code: &str) {
    let line = format!(
        "{} | {} | http={} | code={}\n",
        chrono_now(),
        step,
        status,
        code
    );
    // Primero junto al ejecutable; si no se puede escribir (p. ej.
    // `C:\Program Files` sin admin), en la carpeta de datos.
    let mut done = append_line(&exe_log_path(), &line);
    if !done {
        done = append_line(&log_path(), &line);
    }
    let _ = done;
}

fn append_line(path: &std::path::Path, line: &str) -> bool {
    if let Some(p) = path.parent() {
        if std::fs::create_dir_all(p).is_err() {
            return false;
        }
    }
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .is_ok()
}

pub fn note(step: &str, text: &str) {
    let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
    log(step, &clean, "-");
}

fn chrono_now() -> String {
    // Fecha/hora local sin dependencias: usa SystemTime -> formato simple.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 2026-01-01 00:00 UTC = 1767225600. Suma +2h (Madrid, octubre) de forma fija.
    let local = secs + 2 * 3600;
    let (mut days, rem) = (local / 86400, local % 86400);
    let (mut y, mut m) = (1970i64, 1i64);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if days < yd {
            break;
        }
        days -= yd;
        y += 1;
    }
    let ml = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    loop {
        let mut dim = ml[(m - 1) as usize];
        if m == 2 && ((y % 4 == 0 && y % 100 != 0) || y % 400 == 0) {
            dim = 29;
        }
        if days < dim {
            break;
        }
        days -= dim;
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        days + 1,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// --- DPAPI (sin dependencias extra) ---

fn m(lang: &str, es: &str, en: &str) -> String {
    if lang == "en" { en.into() } else { es.into() }
}

fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;
    let mut src_vec = data.to_vec();
    let src = CRYPT_INTEGER_BLOB {
        cbData: src_vec.len() as u32,
        pbData: src_vec.as_mut_ptr(),
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    let r = unsafe {
        if protect {
            CryptProtectData(&src, PCWSTR::null(), None, None, None, 0, &mut out)
        } else {
            CryptUnprotectData(&src, None, None, None, None, 0, &mut out)
        }
    };
    if r.is_err() {
        return Err(m(&crate::cur_lang(), "DPAPI falló", "DPAPI failed").into());
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(HLOCAL(out.pbData as *mut std::ffi::c_void));
    }
    Ok(bytes)
}

pub fn save_tokens(tokens: &serde_json::Value) -> Result<(), String> {
    let raw = serde_json::to_vec(tokens).map_err(|e| e.to_string())?;
    let enc = dpapi(&raw, true)?;
    if let Some(p) = store_path().parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(store_path(), enc).map_err(|e| e.to_string())
}

pub fn load_tokens() -> Result<serde_json::Value, String> {
    let enc = std::fs::read(store_path()).map_err(|_| "Sin vincular".to_string())?;
    let raw = dpapi(&enc, false)?;
    serde_json::from_slice(&raw).map_err(|e| e.to_string())
}

pub fn forget() {
    std::fs::remove_file(store_path()).ok();
}

pub fn is_linked() -> bool {
    store_path().exists()
}

pub fn profile_token() -> Result<String, String> {
    load_tokens()?
        .get("profile")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Sin vincular: pulsa Vincular cuenta Meta.".to_string())
}

// --- Peticiones a Meta ---

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("QD/1.0")
        .build()
        .expect("http client")
}

pub fn post(url: &str, fields: &[(&str, &str)], step: &str) -> Result<serde_json::Value, String> {
    let mut form = std::collections::HashMap::new();
    for (k, v) in fields {
        form.insert(*k, *v);
    }
    let resp = client().post(url).form(&form).send();
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            log(step, "red", "-");
            return Err(format!("{step}: {} ({e})", m(&crate::cur_lang(), "Meta no respondió", "Meta did not respond")));
        }
    };
    let status = resp.status().as_u16();
    let data: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    let code = data
        .get("error")
        .and_then(|e| e.get("code"))
        .or_else(|| {
            data.get("errors")
                .and_then(|e| e.get(0))
                .and_then(|e| e.get("code"))
        })
        .map(|c| c.to_string())
        .unwrap_or_else(|| "None".into());
    log(step, &status.to_string(), &code);
    let bad = !data.is_object() || data.get("error").is_some() || data.get("errors").is_some();
    if bad && code == "190" {
        return Err(format!(
            "{step}: {}", m(&crate::cur_lang(), "tu sesión de Meta caducó. Desvincula y vuelve a vincular.", "your Meta session expired. Unlink and link again.")
        ));
    }
    if bad && status == 500 {
        return Err(format!(
            "{step}: {} ({code}). {}", m(&crate::cur_lang(), "Meta devolvió 500", "Meta returned 500"), m(&crate::cur_lang(), "Suele ser de la cuenta: completa la verificación pendiente en meta.com, cierra sesiones antiguas y vuelve a vincular.", "It is usually the account: complete the pending verification at meta.com, close old sessions and link again.")
        ));
    }
    if bad {
        return Err(format!(
            "{step}: {} (http={status}, code={code}). {}", m(&crate::cur_lang(), "Meta rechazó la petición", "Meta rejected the request"), m(&crate::cur_lang(), "Desvincula y vuelve a vincular.", "Unlink and link again.")
        ));
    }
    Ok(data)
}

pub fn query(token: &str, doc: &str, variables: &str, step: &str) -> Result<serde_json::Value, String> {
    post(
        GRAPH,
        &[("access_token", token), ("doc_id", doc), ("variables", variables)],
        step,
    )
}

// --- Biblioteca real de la cuenta ---

fn nodes(v: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    let Some(o) = v.as_object() else {
        return vec![];
    };
    if let Some(n) = o.get("nodes").and_then(|n| n.as_array()) {
        return n.clone();
    }
    o.get("edges")
        .and_then(|e| e.as_array())
        .map(|edges| {
            edges
                .iter()
                .filter_map(|e| e.get("node").cloned())
                .collect()
        })
        .unwrap_or_default()
}

fn quest_app(item: &serde_json::Value) -> bool {
    let binary = item.get("latest_supported_binary");
    let platform = item
        .get("platform")
        .and_then(|p| p.as_str())
        .or_else(|| {
            binary.and_then(|b| b.get("platform")).and_then(|p| p.as_str())
        })
        .unwrap_or("")
        .to_uppercase();
    let devices: Vec<String> = ["supported_hmd_platforms", "supported_hmd_types", "targeted_devices"]
        .iter()
        .filter_map(|k| item.get(*k))
        .filter_map(|v| v.as_array())
        .flatten()
        .filter_map(|d| d.as_str().map(|s| s.to_uppercase()))
        .collect();
    let quest = devices
        .iter()
        .any(|d| QUEST.iter().any(|q| d.contains(q)));
    platform == "ANDROID_6DOF"
        || (platform == "ANDROID" && quest)
        || (platform.is_empty()
            && quest
            && binary
                .and_then(|b| b.get("__typename"))
                .and_then(|t| t.as_str())
                == Some("AndroidBinary"))
}

#[derive(Clone, Debug)]
pub struct LibGame {
    pub id: String,
    pub name: String,
    pub package: String,
    pub publisher: String,
    pub genres: String,
}

pub fn fetch_library(token: &str) -> Result<(String, String, Vec<LibGame>), String> {
    let resp = query(token, LIBRARY_DOC, "{}", "biblioteca")?;
    let viewer = resp.get("data").and_then(|d| d.get("viewer"));
    let user = viewer.and_then(|v| v.get("user"));
    let conn = user
        .and_then(|u| u.get("active_entitlements"))
        .or_else(|| viewer.and_then(|v| v.get("active_entitlements")));
    let Some(conn) = conn else {
        return Err(m(&crate::cur_lang(), "Meta no devolvió tu biblioteca. Vuelve a vincular.", "Meta did not return your library. Link again.").into());
    };
    let items: Vec<serde_json::Value> = nodes(conn)
        .into_iter()
        .filter_map(|e| e.get("item").cloned())
        .collect();
    let mut games = vec![];
    for i in items {
        let Some(id) = i.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if !quest_app(&i) {
            continue;
        }
        let binary = i.get("latest_supported_binary");
        games.push(LibGame {
            id: id.to_string(),
            name: i
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            package: binary
                .and_then(|b| b.get("package_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            publisher: i
                .get("publisher_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            genres: i
                .get("genre_names")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|g| g.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
        });
    }
    let alias = user
        .and_then(|u| u.get("alias"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let uid = user
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_u64().map(|n| n.to_string())))
        .unwrap_or_default();
    Ok((alias, uid, games))
}

pub fn item_entitlement(token: &str, user_id: &str, app_id: &str) -> Option<bool> {
    let url = format!("https://graph.oculus.com/{app_id}/verify_entitlement");
    let step = format!("juego {app_id}");
    let resp = post(&url, &[("access_token", token), ("user_id", user_id)], &step).ok()?;
    match resp.get("success") {
        Some(serde_json::Value::Bool(true)) => Some(true),
        Some(serde_json::Value::Bool(false)) => Some(false),
        _ => None,
    }
}

// --- Plan de descarga (APK + datos + opcionales) ---

#[derive(Clone, Debug)]
pub struct PlanFile {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub size: u64,
    pub iap: bool,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub name: String,
    pub package: String,
    pub version: String,
    pub files: Vec<PlanFile>,
    pub optional: Vec<PlanFile>,
}

fn str_of(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn u64_of(v: Option<&serde_json::Value>) -> u64 {
    v.and_then(|x| x.as_u64())
        .or_else(|| v.and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

pub fn plan(token: &str, app_id: &str) -> Result<Plan, String> {
    let step = format!("juego {app_id}");
    let vars = serde_json::json!({ "applicationID": app_id }).to_string();
    let node = query(token, "2885322071572384", &vars, &step)?
        .get("data")
        .and_then(|d| d.get("node"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut application = node.clone();
    let mut released = serde_json::Value::Null;
    if !quest_app(&application) {
        let vars = serde_json::json!({ "itemId": app_id, "hmdType": "EUREKA" }).to_string();
        let listing = query(token, "6549406941839522", &vars, &step)?
            .get("data")
            .and_then(|d| d.get("item"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if listing.get("id").and_then(|v| v.as_str()).unwrap_or("") != app_id
            || !quest_app(&listing)
        {
            return Err(m(&crate::cur_lang(), "Meta no devolvió un build de Quest para este juego.", "Meta did not return a Quest build for this game.").into());
        }
        released = listing.get("latest_supported_binary").cloned().unwrap_or(serde_json::Value::Null);
        application = listing;
    }
    let mut builds: Vec<serde_json::Value> = ["primary_binaries", "binaries"]
        .iter()
        .filter_map(|k| node.get(*k))
        .flat_map(nodes)
        .filter(|b| {
            b.get("platform").and_then(|p| p.as_str()) != Some("PC")
                && b.get("__typename").and_then(|t| t.as_str()) != Some("RiftBinary")
        })
        .collect();
    builds.sort_by_key(|b| {
        std::cmp::Reverse(
            b.get("version_code")
                .or_else(|| b.get("versionCode"))
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                .unwrap_or(0),
        )
    });
    if released.get("id").is_some() {
        let rid = released.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let current = builds
            .iter()
            .find(|b| b.get("id") == Some(&rid))
            .cloned()
            .unwrap_or_else(|| released.clone());
        let mut merged = current.as_object().cloned().unwrap_or_default();
        if let Some(ro) = released.as_object() {
            for (k, v) in ro {
                merged.insert(k.clone(), v.clone());
            }
        }
        let merged_v = serde_json::Value::Object(merged);
        let rest: Vec<serde_json::Value> = builds
            .into_iter()
            .filter(|b| b.get("id") != Some(&rid))
            .collect();
        builds = std::iter::once(merged_v).chain(rest).collect();
    }
    let Some(selected) = builds.into_iter().next() else {
        return Err(m(&crate::cur_lang(), "Meta no devolvió ningún build descargable de Quest.", "Meta did not return any downloadable Quest build.").into());
    };
    let sid = selected.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let vars = serde_json::json!({ "binaryID": sid }).to_string();
    let detail = query(token, "4734929166632773", &vars, &step)?
        .get("data")
        .and_then(|d| d.get("node"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut binary = selected.as_object().cloned().unwrap_or_default();
    if let Some(do_) = detail.as_object() {
        for (k, v) in do_ {
            binary.insert(k.clone(), v.clone());
        }
    }
    let binary = serde_json::Value::Object(binary);
    if binary.get("platform").and_then(|p| p.as_str()) == Some("PC")
        || binary
            .get("package_name")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .is_empty()
    {
        return Err(m(&crate::cur_lang(), "Lo que devolvió Meta no es un APK de Quest.", "What Meta returned is not a Quest APK.").into());
    }
    let mk = |id: String, name: String, uri: String, size: u64, iap: bool| PlanFile {
        id,
        name,
        uri,
        size,
        iap,
    };
    let mut files = vec![mk(
        str_of(binary.get("id")),
        "base.apk".into(),
        str_of(binary.get("uri")),
        u64_of(binary.get("size")),
        false,
    )];
    if let Some(obb) = binary.get("obb_binary") {
        if obb.get("id").is_some() {
            files.push(mk(
                str_of(obb.get("id")),
                str_of(obb.get("file_name")),
                str_of(obb.get("uri")),
                u64_of(obb.get("size")),
                false,
            ));
        }
    }
    let assets_node = binary.get("asset_files").cloned().unwrap_or(serde_json::Value::Null);
    let assets = nodes(&assets_node);
    let count = assets_node.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    if count > assets.len() as u64 {
        return Err(m(&crate::cur_lang(), "Meta devolvió una lista incompleta; no se descarga nada.", "Meta returned an incomplete list; nothing will be downloaded.").into());
    }
    let mut optional = vec![];
    for a in assets {
        let fname = str_of(a.get("file_name"));
        if a.get("is_required") == Some(&serde_json::Value::Bool(false))
            || a.get("iap_item").is_some()
        {
            optional.push(mk(
                str_of(a.get("id")),
                fname,
                str_of(a.get("uri")),
                u64_of(a.get("size")),
                a.get("iap_item").is_some(),
            ));
            continue;
        }
        if files.iter().any(|f| f.name == fname) {
            continue;
        }
        files.push(mk(
            str_of(a.get("id")),
            fname,
            str_of(a.get("uri")),
            u64_of(a.get("size")),
            false,
        ));
    }
    Ok(Plan {
        name: application
            .get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| binary.get("package_name").and_then(|v| v.as_str()))
            .unwrap_or("?")
            .to_string(),
        package: str_of(binary.get("package_name")),
        version: str_of(binary.get("version")),
        files,
        optional,
    })
}

pub fn delivery_token(profile: &str) -> Result<String, String> {
    let url = format!("https://graph.oculus.com/authenticate_application?app_id={DELIVERY_APP}");
    let r = post(&url, &[("access_token", profile)], "descarga")?;
    r.get("access_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| m(&crate::cur_lang(), "Meta no devolvió el token de descarga. Vuelve a vincular.", "Meta did not return the download token. Link again."))
}

fn enc(v: &str) -> String {
    let mut o = String::new();
    for b in v.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(*b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn dec_pair(qs: &str) -> Vec<(String, String)> {
    qs.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (url_decode(k), url_decode(v)))
        .collect()
}

pub fn file_url(file: &PlanFile, token: &str) -> Result<String, String> {
    if !file.uri.is_empty() {
        let (base, qs) = match file.uri.split_once('?') {
            Some((b, q)) => (b.to_string(), q.to_string()),
            None => (file.uri.clone(), String::new()),
        };
        let host_ok = base.starts_with("https://securecdn")
            && base.contains(".oculus.com")
            && base.ends_with("/binaries/download/");
        if host_ok {
            let mut pairs = dec_pair(&qs);
            pairs.retain(|(k, _)| k != "access_token");
            pairs.push(("access_token".into(), token.into()));
            let q = pairs
                .iter()
                .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
                .collect::<Vec<_>>()
                .join("&");
            return Ok(format!("{base}?{q}"));
        }
        return Ok(file.uri.clone());
    }
    if !file.id.chars().all(|c| c.is_ascii_digit()) || file.id.is_empty() {
        return Err(m(&crate::cur_lang(), "Meta devolvió un id de archivo inválido.", "Meta returned an invalid file id.").into());
    }
    Ok(format!(
        "https://securecdn.oculus.com/binaries/download/?id={}&access_token={}",
        file.id, token
    ))
}

// --- Ventana de login (Edge desechable + DevTools) ---

fn free_port() -> std::io::Result<u16> {
    let s = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(s.local_addr()?.port())
}

pub(crate) fn edge_path() -> Result<String, String> {
    for p in [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ] {
        if std::path::Path::new(p).exists() {
            return Ok(p.to_string());
        }
    }
    Err(m(
        &crate::cur_lang(),
        "Instala Microsoft Edge para vincular la cuenta.",
        "Install Microsoft Edge to link the account.",
    )
    .into())
}

// Cliente websocket mínimo (solo texto) para las DevTools de Edge.
pub(crate) struct Ws {
    pub(crate) stream: TcpStream,
}

impl Ws {
    pub(crate) fn connect(host: &str, port: u16, path: &str) -> Result<Self, String> {
        use std::collections::hash_map::DefaultHasher;
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            std::time::SystemTime::now().hash(&mut h);
            base64_key(h.finish())
        };
        let mut stream =
            TcpStream::connect((host, port)).map_err(|e| format!("DevTools: {e}"))?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut resp = vec![0u8; 4096];
        let mut got = 0usize;
        loop {
            let n = stream.read(&mut resp[got..]).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("Las DevTools de Edge no respondieron.".into());
            }
            got += n;
            if end_of_headers(&resp[..got]) {
                break;
            }
        }
        let head = String::from_utf8_lossy(&resp[..got]);
        if !head.lines().next().unwrap_or("").contains(" 101 ") {
            return Err(m(&crate::cur_lang(), "Las DevTools de Edge rechazaron la conexión.", "Edge DevTools rejected the connection.").into());
        }
        Ok(Self { stream })
    }

    pub(crate) fn send_text(&mut self, text: &str) -> Result<(), String> {
        let data = text.as_bytes();
        let mut frame = vec![0x81u8];
        // Máscara obligatoria del cliente.
        let mask = [(data.len() as u8).wrapping_mul(31).wrapping_add(17), 0x5A, 0xA5, 0x3C];
        if data.len() < 126 {
            frame.push(0x80 | data.len() as u8);
        } else if data.len() < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(data.len() as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(data.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(
            data.iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % 4]),
        );
        self.stream.write_all(&frame).map_err(|e| e.to_string())
    }

    pub(crate) fn recv_text(&mut self) -> Result<String, String> {
        let mut out = vec![];
        loop {
            let (fin, opcode, payload) = self.recv_frame()?;
            match opcode {
                0x8 => return Err(m(&crate::cur_lang(), "La ventana de Meta se cerró antes de terminar.", "The Meta window closed before finishing.").into()),
                0x9 => {
                    // pong
                    let mut pong = vec![0x8Au8, payload.len() as u8];
                    pong.extend_from_slice(&payload);
                    self.stream.write_all(&pong).map_err(|e| e.to_string())?;
                    continue;
                }
                0x1 | 0x0 | 0xA => {
                    out.extend_from_slice(&payload);
                }
                _ => {}
            }
            if fin {
                break;
            }
        }
        String::from_utf8(out).map_err(|_| m(&crate::cur_lang(), "Respuesta inválida.", "Invalid response.").into())
    }

    fn rd(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match self.stream.read(&mut buf[got..]) {
                Ok(0) => return Err(m(&crate::cur_lang(), "La ventana de Meta se cerró.", "The Meta window closed.").into()),
                Ok(k) => got += k,
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(buf)
    }

    fn recv_frame(&mut self) -> Result<(bool, u8, Vec<u8>), String> {
        let h = self.rd(2)?;
        let fin = h[0] & 0x80 != 0;
        let opcode = h[0] & 0x0F;
        let mut len = (h[1] & 0x7F) as u64;
        if len == 126 {
            len = u16::from_be_bytes(self.rd(2)?[..].try_into().map_err(|_| "frame".to_string())?) as u64;
        } else if len == 127 {
            len = u64::from_be_bytes(self.rd(8)?[..].try_into().map_err(|_| "frame".to_string())?);
        }
        let mask = if h[1] & 0x80 != 0 { Some(self.rd(4)?) } else { None };
        let mut payload = self.rd(len as usize)?;
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }
        Ok((fin, opcode, payload))
    }
}

fn base64_key(n: u64) -> String {
    const B: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = n.to_be_bytes();
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let mut v = 0u32;
        for b in c {
            v = (v << 8) | *b as u32;
        }
        v <<= 8 * (3 - c.len());
        for i in 0..4 {
            if i <= c.len() {
                out.push(B[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn end_of_headers(buf: &[u8]) -> bool {
    buf.windows(4).any(|w| w == b"\r\n\r\n")
}

pub(crate) fn wait_callback(port: u16, deadline: std::time::Instant) -> Result<String, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(m(&crate::cur_lang(), "La ventana de Meta no se abrió.", "The Meta window did not open.").into());
        }
        let pages: serde_json::Value = http
            .get(format!("http://127.0.0.1:{port}/json"))
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(serde_json::Value::Null);
        let page = pages
            .as_array()
            .and_then(|a| a.iter().find(|p| p.get("type").and_then(|t| t.as_str()) == Some("page")))
            .cloned();
        if let Some(p) = page {
            let ws_url = p
                .get("webSocketDebuggerUrl")
                .and_then(|u| u.as_str())
                .ok_or("Sin DevTools.".to_string())?;
            // ws://127.0.0.1:port/devtools/page/xxx
            let path = ws_url.splitn(4, '/').nth(3).map(|s| format!("/{s}")).unwrap_or_default();
            let mut ws = Ws::connect("127.0.0.1", port, &path)?;
            ws.stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .map_err(|e| e.to_string())?;
            ws.send_text(r#"{"id":1,"method":"Page.enable"}"#)?;
            ws.send_text(r#"{"id":2,"method":"Network.enable"}"#)?;
            loop {
                if std::time::Instant::now() > deadline {
                    return Err(m(&crate::cur_lang(), "El inicio de sesión caducó.", "Sign-in expired.").into());
                }
                let text = match ws.recv_text() {
                    Ok(t) => t,
                    Err(e) if e.contains("timed out") || e.contains("os error 10060") || e.contains("WouldBlock") => continue,
                    Err(e) => return Err(e),
                };
                let ev: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
                let params = ev.get("params");
                let mut candidates = vec![
                    params.and_then(|p| p.get("url")).and_then(|u| u.as_str()).unwrap_or(""),
                    params
                        .and_then(|p| p.get("request"))
                        .and_then(|r| r.get("url"))
                        .and_then(|u| u.as_str())
                        .unwrap_or(""),
                    params
                        .and_then(|p| p.get("frame"))
                        .and_then(|r| r.get("url"))
                        .and_then(|u| u.as_str())
                        .unwrap_or(""),
                ];
                let mut locs = vec![];
                for key in ["redirectResponse", "response"] {
                    if let Some(hdrs) = params
                        .and_then(|p| p.get(key))
                        .and_then(|r| r.get("headers"))
                        .and_then(|h| h.as_object())
                    {
                        for (k, v) in hdrs {
                            if k.to_lowercase() == "location" {
                                if let Some(s) = v.as_str() {
                                    locs.push(s);
                                }
                            }
                        }
                    }
                }
                candidates.extend(locs);
                for url in candidates {
                    if url.starts_with("oculus://") || url.starts_with("oculus-client://") {
                        return Ok(url.to_string());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn sha256_hex(data: &[u8]) -> String {
    // SHA-256 manual (sin dependencias): solo se usa para el reto SSO.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bitlen = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn parse_query(qs: &str, key: &str) -> String {
    url_decode(
        &qs.split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .unwrap_or_default(),
    )
}

fn url_decode(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.as_bytes().iter().peekable();
    while let Some(&b) = it.next() {
        if b == b'%' {
            let h: Vec<u8> = it.by_ref().take(2).copied().collect();
            if h.len() == 2 {
                if let Ok(hex) = std::str::from_utf8(&h) {
                    if let Ok(v) = u8::from_str_radix(hex, 16) {
                        out.push(v as char);
                        continue;
                    }
                }
                out.push('%');
                out.extend(h.iter().map(|c| *c as char));
            } else {
                out.push('%');
            }
        } else if b == b'+' {
            out.push(' ');
        } else {
            out.push(b as char);
        }
    }
    // Nota: %XX multibyte UTF-8 queda como chars sueltos; el token SSO es ASCII.
    out
}

pub fn sign_in() -> Result<String, String> {
    let started = post(
        &format!("{META}/webview_tokens_query"),
        &[("access_token", CLIENT)],
        "inicio (desafío)",
    )?;
    let challenge = started.get("native_sso_token").and_then(|v| v.as_str()).unwrap_or("");
    let etoken = started.get("native_sso_etoken").and_then(|v| v.as_str()).unwrap_or("");
    if challenge.is_empty() || etoken.is_empty() {
        return Err(m(&crate::cur_lang(), "Meta no devolvió el desafío de inicio de sesión.", "Meta did not return the sign-in challenge.").into());
    }
    let confirm = format!(
        "https://auth.meta.com/native_sso/confirm?native_app_id={FRL_APP}&source_app_id={FRL_APP}&native_sso_etoken={}",
        enc(etoken)
    );
    let port = free_port().map_err(|e| e.to_string())?;
    let profile = std::env::temp_dir().join(format!("qd-meta-signin-{}", std::process::id()));
    std::fs::create_dir_all(&profile).ok();
    // Login en el navegador predeterminado (Chromium, Firefox o Edge).
    let (mut child, callback) = crate::browser::login(&confirm, port, &profile)?;
    let _ = child.kill();
    std::thread::sleep(Duration::from_secs(1));
    std::fs::remove_dir_all(&profile).ok();
    let qs = callback.split_once('?').map(|(_, q)| q).unwrap_or("");
    let expected = &sha256_hex(challenge.as_bytes())[..16];
    if parse_query(qs, "token") != expected {
        return Err(m(&crate::cur_lang(), "La respuesta no coincide con esta sesión.", "The response does not match this session.").into());
    }
    let blob = parse_query(qs, "blob");
    if blob.is_empty() {
        return Err(m(&crate::cur_lang(), "Meta no devolvió credenciales.", "Meta did not return credentials.").into());
    }
    let account = post(
        &format!("{META}/webview_blobs_decrypt"),
        &[("access_token", CLIENT), ("blob", &blob), ("request_token", challenge)],
        "descifrado",
    )?
    .get("access_token")
    .and_then(|v| v.as_str())
    .map(|s| s.to_string())
    .ok_or(m(&crate::cur_lang(), "Meta no devolvió el token de cuenta.", "Meta did not return the account token."))?;
    let vars = serde_json::json!({ "app_id": PROFILE_APP }).to_string();
    let result = post(
        &format!("{META}/graphql"),
        &[("access_token", &account), ("doc_id", PROFILE_DOC), ("variables", &vars)],
        "token de perfil",
    )?;
    let profile_token = result
        .get("data")
        .and_then(|d| d.get("xfr_create_profile_token"))
        .and_then(|x| x.get("profile_tokens"))
        .and_then(|t| t.get(0))
        .and_then(|t| t.get("access_token"))
        .and_then(|v| v.as_str())
        .ok_or(m(&crate::cur_lang(), "Meta no devolvió el token del perfil Quest.", "Meta did not return the Quest profile token."))?;
    save_tokens(&serde_json::json!({ "profile": profile_token }))?;
    let (alias, _, _) = fetch_library(profile_token)?;
    Ok(alias)
}
