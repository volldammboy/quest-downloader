//! Navegador predeterminado para el inicio de sesión.
//!
//! - Chromium (Edge, Chrome, Brave…): se lanza con depuración remota y se
//!   reutiliza el flujo CDP existente (`meta_auth::wait_callback`).
//! - Firefox: protocolo WebDriver BiDi (`/session` + `network.beforeRequestSent`).
//! - Si el predeterminado falla o no es compatible: vuelta a Edge.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::meta_auth::{wait_callback, Ws};

pub enum DefaultBrowser {
    Chromium(String),
    Firefox(String),
    Other,
}

fn reg_sz(hive: windows::Win32::System::Registry::HKEY, subkey: &str, value: Option<&str>) -> Option<String> {
    use windows::Win32::System::Registry::*;
    use windows::core::PCWSTR;
    let sub: Vec<u16> = subkey.encode_utf16().chain([0]).collect();
    unsafe {
        let mut h = HKEY::default();
        if RegOpenKeyExW(hive, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut h).0 != 0 {
            return None;
        }
        let name: Vec<u16> = match value {
            Some(v) => v.encode_utf16().chain([0]).collect(),
            None => vec![0],
        };
        let mut len = 0u32;
        if RegQueryValueExW(h, PCWSTR(name.as_ptr()), None, None, None, Some(&mut len)).0 != 0
            || len == 0
            || len > 32768
        {
            RegCloseKey(h);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = RegQueryValueExW(
            h,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
        .0 == 0;
        RegCloseKey(h);
        if !ok {
            return None;
        }
        let u16s: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some(
            String::from_utf16_lossy(&u16s)
                .trim_matches('\0')
                .trim()
                .to_string(),
        )
    }
}

fn exe_of(cmd: &str) -> Option<String> {
    let t = cmd.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix('"') {
        return rest.split('"').next().map(|s| s.to_string());
    }
    t.to_lowercase()
        .find(".exe")
        .map(|i| t[..i + 4].trim_matches('"').to_string())
}

fn exists(p: &str) -> bool {
    !p.is_empty() && std::path::Path::new(p).exists()
}

fn find_firefox() -> Option<String> {
    [
        r"C:\Program Files\Mozilla Firefox\firefox.exe",
        r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
    ]
    .iter()
    .find(|p| exists(p))
    .map(|s| s.to_string())
}

fn find_chromium() -> Option<String> {
    [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    .iter()
    .find(|p| exists(p))
    .map(|s| s.to_string())
}

pub fn detect() -> DefaultBrowser {
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    if let Some(prog) = reg_sz(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
        Some("ProgId"),
    ) {
        let key = format!(r"{prog}\shell\open\command");
        if let Some(cmd) = reg_sz(HKEY_CLASSES_ROOT, &key, None) {
            if let Some(exe) = exe_of(&cmd) {
                if exists(&exe) {
                    let low = exe.to_lowercase();
                    if low.contains("firefox") {
                        return DefaultBrowser::Firefox(exe);
                    }
                    if ["chrome", "msedge", "brave", "vivaldi", "opera", "chromium"]
                        .iter()
                        .any(|k| low.contains(k))
                    {
                        return DefaultBrowser::Chromium(exe);
                    }
                    return DefaultBrowser::Other;
                }
            }
        }
        let low = prog.to_lowercase();
        if low.contains("firefox") {
            if let Some(exe) = find_firefox() {
                return DefaultBrowser::Firefox(exe);
            }
        }
        if ["chrome", "msedge", "brave", "vivaldi", "opera"]
            .iter()
            .any(|k| low.contains(k))
        {
            if let Some(exe) = find_chromium() {
                return DefaultBrowser::Chromium(exe);
            }
        }
        return DefaultBrowser::Other;
    }
    if let Some(exe) = find_chromium() {
        return DefaultBrowser::Chromium(exe);
    }
    if let Some(exe) = find_firefox() {
        return DefaultBrowser::Firefox(exe);
    }
    DefaultBrowser::Other
}

fn launch_chromium(
    exe: &str,
    port: u16,
    profile: &std::path::Path,
    url: &str,
) -> std::io::Result<Child> {
    Command::new(exe)
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--new-window")
        .arg(url)
        .spawn()
}

fn launch_firefox(
    exe: &str,
    port: u16,
    profile: &std::path::Path,
) -> std::io::Result<Child> {
    // Sin URL: la navegación la ordena BiDi (una sola ventana).
    Command::new(exe)
        .arg("-new-instance")
        .arg("--profile")
        .arg(profile)
        .arg("--remote-debugging-port")
        .arg(port.to_string())
        .arg("about:blank")
        .spawn()
}

fn bidi_cmd(ws: &mut Ws, id: u32, method: &str, params: &str) -> Result<serde_json::Value, String> {
    ws.send_text(&format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#))?;
    loop {
        let text = ws.recv_text()?;
        let v: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        if v.get("id").and_then(|x| x.as_u64()) == Some(id as u64) {
            return Ok(v);
        }
        if v.get("type").and_then(|t| t.as_str()) == Some("error") {
            return Err(v
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("BiDi error")
                .to_string());
        }
        // eventos que lleguen antes de tiempo se ignoran aquí
    }
}

fn bidi_ok(r: &serde_json::Value) -> bool {
    r.get("result").is_some() || r.get("type").and_then(|t| t.as_str()) == Some("success")
}

/// Espera el redirect oculus:// vía WebDriver BiDi (Firefox).
fn bidi_wait(port: u16, deadline: Instant, url: &str) -> Result<String, String> {
    let mut ws = loop {
        if Instant::now() > deadline {
            return Err("BiDi: sin respuesta.".into());
        }
        match Ws::connect("127.0.0.1", port, "/session") {
            Ok(mut w) => {
                w.stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .map_err(|e| e.to_string())?;
                break w;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    };
    let r = bidi_cmd(&mut ws, 1, "session.new", r#"{"capabilities":{}}"#)?;
    if !bidi_ok(&r) {
        return Err("BiDi: sesión no creada.".into());
    }
    let r = bidi_cmd(
        &mut ws,
        2,
        "session.subscribe",
        r#"{"events":["network.beforeRequestSent","browsingContext.navigationStarted"]}"#,
    )?;
    if !bidi_ok(&r) {
        return Err("BiDi: suscripción rechazada.".into());
    }
    let r = bidi_cmd(&mut ws, 3, "browsingContext.create", r#"{"type":"window"}"#)?;
    let ctx = r
        .get("result")
        .and_then(|x| x.get("context"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if ctx.is_empty() {
        return Err("BiDi: sin ventana.".into());
    }
    let nav = serde_json::json!({ "context": ctx, "url": url }).to_string();
    let r = bidi_cmd(&mut ws, 4, "browsingContext.navigate", &nav)?;
    if !bidi_ok(&r) {
        return Err("BiDi: navegación rechazada.".into());
    }
    loop {
        if Instant::now() > deadline {
            return Err("BiDi: tiempo agotado.".into());
        }
        let text = match ws.recv_text() {
            Ok(t) => t,
            Err(e)
                if e.contains("timed out")
                    || e.contains("os error 10060")
                    || e.contains("WouldBlock") =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        let ev: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        if ev.get("type").and_then(|t| t.as_str()) != Some("event") {
            continue;
        }
        let params = ev.get("params").cloned().unwrap_or(serde_json::Value::Null);
        let mut urls = vec![
            params
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or(""),
            params
                .get("request")
                .and_then(|r| r.get("url"))
                .and_then(|u| u.as_str())
                .unwrap_or(""),
        ];
        for url in urls.drain(..) {
            if url.starts_with("oculus://") || url.starts_with("oculus-client://") {
                return Ok(url.to_string());
            }
        }
    }
}

fn edge_login(
    confirm: &str,
    port: u16,
    profile: &std::path::Path,
    deadline: Instant,
) -> Result<(Child, String), String> {
    let exe = crate::meta_auth::edge_path()?;
    let mut child = launch_chromium(&exe, port, profile, confirm).map_err(|e| {
        format!(
            "{}: {e}",
            crate::tr(&crate::cur_lang(), "No se pudo abrir el navegador")
        )
    })?;
    match wait_callback(port, deadline) {
        Ok(url) => Ok((child, url)),
        Err(e) => {
            let _ = child.kill();
            Err(e)
        }
    }
}

/// Abre el login en el navegador predeterminado y espera el redirect.
/// Si falla, reintenta con Edge. Devuelve (proceso, url de retorno).
pub fn login(
    confirm: &str,
    port: u16,
    profile: &std::path::Path,
) -> Result<(Child, String), String> {
    let deadline = Instant::now() + Duration::from_secs(600);
    match detect() {
        DefaultBrowser::Firefox(exe) => {
            match launch_firefox(&exe, port, profile) {
                Ok(mut child) => match bidi_wait(port, deadline, confirm) {
                    Ok(url) => Ok((child, url)),
                    Err(_) => {
                        let _ = child.kill();
                        edge_login(confirm, port, profile, deadline)
                    }
                },
                Err(_) => edge_login(confirm, port, profile, deadline),
            }
        }
        DefaultBrowser::Chromium(exe) => {
            match launch_chromium(&exe, port, profile, confirm) {
                Ok(mut child) => match wait_callback(port, deadline) {
                    Ok(url) => Ok((child, url)),
                    Err(_) => {
                        let _ = child.kill();
                        edge_login(confirm, port, profile, deadline)
                    }
                },
                Err(_) => edge_login(confirm, port, profile, deadline),
            }
        }
        DefaultBrowser::Other => edge_login(confirm, port, profile, deadline),
    }
}
