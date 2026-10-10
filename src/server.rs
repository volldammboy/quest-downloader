//! Servidor HTTP en la LAN para el APK del visor.
//!
//! - `GET /api/games` → JSON con lo descargado (carpetas con `base.apk`).
//! - `GET /f/<carpeta>/<fichero>` → APK/OBB con soporte `Range`.
//! - `GET /cover/<id>.jpg` → portada en caché.
//! Todo exige `?t=<token>` o cabecera `X-QD-Token` (se muestra en Descargas).

use std::io::Read;
use std::sync::Mutex;

pub const PORT: u16 = 4690;

struct Ctl {
    running: bool,
    stop: bool,
    ip: String,
    token: String,
    error: String,
}

static CTL: Mutex<Ctl> = Mutex::new(Ctl {
    running: false,
    stop: false,
    ip: String::new(),
    token: String::new(),
    error: String::new(),
});

pub struct SrvInfo {
    pub running: bool,
    pub ip: String,
    pub token: String,
    pub error: String,
}

pub fn status() -> SrvInfo {
    let c = CTL.lock().unwrap();
    SrvInfo {
        running: c.running,
        ip: c.ip.clone(),
        token: c.token.clone(),
        error: c.error.clone(),
    }
}

fn safe(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let s = s.trim();
    if s.is_empty() {
        "juego".into()
    } else {
        s.to_string()
    }
}

fn token() -> String {
    let old = crate::store::get_setting("srv_token");
    if !old.trim().is_empty() {
        return old;
    }
    let mut buf = [0u8; 16];
    unsafe {
        use windows::Win32::Security::Cryptography::BCryptGenRandom;
        use windows::Win32::Security::Cryptography::BCRYPT_USE_SYSTEM_PREFERRED_RNG;
        if BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG).is_ok() {
            let t: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            crate::store::set_setting("srv_token", &t);
            return t;
        }
    }
    let t = format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    );
    crate::store::set_setting("srv_token", &t);
    t
}

pub fn local_ip() -> String {
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if s.connect("192.168.1.1:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                return a.ip().to_string();
            }
        }
    }
    "127.0.0.1".into()
}

fn authorized(req: &tiny_http::Request, token: &str) -> bool {
    if let Some(h) = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-QD-Token"))
    {
        if h.value.as_str() == token {
            return true;
        }
    }
    let url = req.url();
    if let Some(q) = url.split_once('?').map(|(_, q)| q) {
        for kv in q.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                if k == "t" && v == token {
                    return true;
                }
            }
        }
    }
    false
}

fn games_json(token: &str) -> String {
    let lib = crate::store::library_all();
    let mut out = String::from("[");
    let mut first = true;
    let root = crate::store::download_dir();
    let entries = std::fs::read_dir(&root).map(|r| r.collect::<Vec<_>>()).unwrap_or_default();
    for e in entries {
        let Ok(e) = e else { continue };
        let dir = e.path();
        if !dir.is_dir() || !dir.join("base.apk").exists() {
            continue;
        }
        let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let g = lib.iter().find(|g| safe(&g.name) == folder);
        let mut files = String::new();
        let mut ff = true;
        let mut add = |n: &str, sz: u64| {
            if !ff {
                files.push(',');
            }
            ff = false;
            files.push_str(&format!(
                "{{\"name\":{},\"size\":{},\"url\":\"/f/{}/{}?t={}\"}}",
                serde_json::json!(n),
                sz,
                folder,
                n,
                token
            ));
        };
        if let Ok(md) = std::fs::metadata(dir.join("base.apk")) {
            add("base.apk", md.len());
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut obbs: Vec<_> = rd
                .flatten()
                .map(|x| x.path())
                .filter(|p| {
                    p.extension().and_then(|x| x.to_str()) == Some("obb") && p.is_file()
                })
                .collect();
            obbs.sort();
            for p in obbs {
                if let (Some(n), Ok(md)) = (p.file_name().and_then(|n| n.to_str()), std::fs::metadata(&p)) {
                    add(n, md.len());
                }
            }
        }
        if !first {
            out.push(',');
        }
        first = false;
        let (name, package, cover) = match g {
            Some(g) => {
                let c = format!("/cover/{}.jpg?t={}", g.id, token);
                let has = crate::store::covers_dir().join(format!("{}.jpg", g.id)).exists();
                (
                    g.name.clone(),
                    g.package.clone(),
                    if has { Some(c) } else { None },
                )
            }
            None => (folder.to_string(), String::new(), None),
        };
        out.push_str(&format!(
            "{{\"id\":{},\"name\":{},\"package\":{},\"cover\":{},\"files\":[{}]}}",
            serde_json::json!(folder),
            serde_json::json!(name),
            serde_json::json!(package),
            serde_json::json!(cover),
            files
        ));
    }
    out.push(']');
    out
}

fn serve_file(req: tiny_http::Request, path: &std::path::Path, mime: &str) {
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        _ => {
            let _ = req.respond(tiny_http::Response::empty(404));
            return;
        }
    };
    let total = meta.len();
    let range = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string());
    let (start, end) = match range.as_deref() {
        Some(r) if r.starts_with("bytes=") => {
            let spec = &r[6..];
            let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
            let s: u64 = a.parse().unwrap_or(0);
            let e: u64 = b.parse::<u64>().map(|v| v.min(total.saturating_sub(1))).unwrap_or(total.saturating_sub(1));
            (s.min(total), e.max(s).min(total.saturating_sub(1)))
        }
        _ => (0, total.saturating_sub(1)),
    };
    if start >= total && total > 0 {
        let resp = tiny_http::Response::empty(416).with_header(
            tiny_http::Header::from_bytes("Content-Range", format!("bytes */{total}")).unwrap(),
        );
        let _ = req.respond(resp);
        return;
    }
    let len = end.saturating_sub(start) + if total > 0 { 1 } else { 0 };
    let f = match std::fs::File::open(path) {
        Ok(mut f) => {
            use std::io::Seek;
            if f.seek(std::io::SeekFrom::Start(start)).is_err() {
                let _ = req.respond(tiny_http::Response::empty(500));
                return;
            }
            f
        }
        Err(_) => {
            let _ = req.respond(tiny_http::Response::empty(404));
            return;
        }
    };
    let partial = start > 0 || end + 1 < total;
    let mut headers = vec![
        tiny_http::Header::from_bytes("Content-Type", mime).unwrap(),
        tiny_http::Header::from_bytes("Accept-Ranges", "bytes").unwrap(),
    ];
    let code = if partial {
        headers.push(
            tiny_http::Header::from_bytes(
                "Content-Range",
                format!("bytes {start}-{end}/{total}"),
            )
            .unwrap(),
        );
        206
    } else {
        200
    };
    let resp = tiny_http::Response::new(
        tiny_http::StatusCode(code),
        headers,
        f.take(len),
        Some(len as usize),
        None,
    );
    let _ = req.respond(resp);
}

fn handle(req: tiny_http::Request, token: &str) {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    if !authorized(&req, token) {
        let _ = req.respond(tiny_http::Response::from_string("forbidden").with_status_code(403));
        return;
    }
    if path == "/api/games" {
        let body = games_json(token);
        let _ = req.respond(
            tiny_http::Response::from_string(body).with_header(
                tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
            ),
        );
        return;
    }
    if let Some(rest) = path.strip_prefix("/f/") {
        let mut parts = rest.splitn(2, '/');
        let (folder, file) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if folder.is_empty() || folder.contains("..") || file.is_empty() || file.contains("..") || file.contains('/') {
            let _ = req.respond(tiny_http::Response::empty(400));
            return;
        }
        let p = crate::store::download_dir().join(folder).join(file);
        let ok_name = file == "base.apk" || file.ends_with(".obb");
        if !ok_name {
            let _ = req.respond(tiny_http::Response::empty(403));
            return;
        }
        serve_file(req, &p, "application/octet-stream");
        return;
    }
    if let Some(rest) = path.strip_prefix("/cover/") {
        if rest.contains("..") || rest.contains('/') {
            let _ = req.respond(tiny_http::Response::empty(400));
            return;
        }
        serve_file(req, &crate::store::covers_dir().join(rest), "image/jpeg");
        return;
    }
    let _ = req.respond(tiny_http::Response::empty(404));
}

/// Arranca (o para, si ya corría) el servidor. Devuelve el estado.
pub fn toggle() -> SrvInfo {
    {
        let c = CTL.lock().unwrap();
        if c.running {
            drop(c);
            stop();
            return status();
        }
    }
    let token = token();
    let ip = local_ip();
    let server = match tiny_http::Server::http(format!("0.0.0.0:{PORT}")) {
        Ok(s) => s,
        Err(e) => {
            let mut c = CTL.lock().unwrap();
            c.error = e.to_string();
            drop(c);
            return status();
        }
    };
    {
        let mut c = CTL.lock().unwrap();
        c.running = true;
        c.stop = false;
        c.ip = ip;
        c.token = token.clone();
        c.error.clear();
    }
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            if CTL.lock().unwrap().stop {
                break;
            }
            // Petición señuelo interna para despertar el bucle al parar.
            if req.url().starts_with("/quit") {
                let _ = req.respond(tiny_http::Response::empty(200));
                break;
            }
            handle(req, &token);
        }
        CTL.lock().unwrap().running = false;
    });
    // Despertar provisional por si el puerto sigue abierto de antes: no-op.
    status()
}

fn stop() {
    CTL.lock().unwrap().stop = true;
    // Señuelo para desbloquear incoming_requests().
    let url = format!("http://127.0.0.1:{PORT}/quit");
    let _ = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .and_then(|c| c.get(&url).send());
    let mut c = CTL.lock().unwrap();
    c.running = false;
}
