//! Sesión web de Meta (meta.com) para el historial de pedidos.
//!
//! La consulta `active_entitlements` (graph.oculus.com) devuelve 500 en
//! algunas cuentas. Alternativa: captura de la página de pedidos con el
//! navegador temporal (autoscroll), sin sesiones guardadas.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const ORDERS_URL: &str =
    "https://www.meta.com/my/orders/?order_type=digital_products&timeFilter=ALL_TIME";

fn cdp_call(
    ws: &mut crate::meta_auth::Ws,
    id: u32,
    method: &str,
    params: &str,
) -> Result<serde_json::Value, String> {
    ws.send_text(&format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#))?;
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > Duration::from_secs(15) {
            return Err("CDP sin respuesta.".into());
        }
        let text = ws.recv_text()?;
        let v: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        if v.get("id").and_then(|x| x.as_u64()) == Some(id as u64) {
            return Ok(v);
        }
    }
}

fn page_ws(port: u16, deadline: Instant) -> Result<(crate::meta_auth::Ws, String), String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    loop {
        if Instant::now() > deadline {
            return Err("Sin pestaña.".into());
        }
        if let Ok(r) = http.get(format!("http://127.0.0.1:{port}/json/list")).send() {
            if let Ok(tabs) = r.json::<serde_json::Value>() {
                if let Some(arr) = tabs.as_array() {
                    let mut best: Option<String> = None;
                    let mut best_url = String::new();
                    let mut fallback: Option<String> = None;
                    let mut fallback_url = String::new();
                    for t in arr {
                        if t.get("type").and_then(|x| x.as_str()) != Some("page") {
                            continue;
                        }
                        let u = t.get("url").and_then(|x| x.as_str()).unwrap_or("");
                        let w = t.get("webSocketDebuggerUrl").and_then(|x| x.as_str()).unwrap_or("");
                        if w.is_empty() || !(u.starts_with("https://") || u.starts_with("http://")) {
                            continue;
                        }
                        if u.contains("meta.com/my/orders") {
                            best = Some(w.into());
                            best_url = u.into();
                            break;
                        }
                        if fallback.is_none() && (u.contains("meta.com") || u.contains("oculus.com")) {
                            fallback = Some(w.into());
                            fallback_url = u.into();
                        }
                    }
                    let (w, url) = match (best, fallback) {
                        (Some(w), _) => (w, best_url.clone()),
                        (None, Some(w)) => (w, fallback_url.clone()),
                        _ => continue,
                    };
                    let _ = url;
                    if let Some(p) = w.split(&format!(":{port}")).nth(1) {
                        let mut ws =
                            crate::meta_auth::Ws::connect("127.0.0.1", port, p)?;
                        ws.stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .map_err(|e| e.to_string())?;
                        return Ok((ws, best_url));
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn eval(ws: &mut crate::meta_auth::Ws, id: &mut u32, expr: &str) -> String {
    *id += 1;
    let params = serde_json::json!({"expression": expr, "returnByValue": true}).to_string();
    cdp_call(ws, *id, "Runtime.evaluate", &params)
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|x| x.get("result"))
                .and_then(|x| x.get("value"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

/// Minimiza la ventana principal del navegador (tras el login): la captura
/// sigue en segundo plano y el usuario solo ve llenarse la Biblioteca.
fn minimize_pid(pid: u32) {
    struct Ctx {
        pid: u32,
        hwnd: isize,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut wpid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut wpid));
        let no_owner = GetWindow(hwnd, GW_OWNER)
            .map(|h| h == HWND(std::ptr::null_mut()))
            .unwrap_or(true);
        if wpid == ctx.pid && IsWindowVisible(hwnd).as_bool() && no_owner {
            ctx.hwnd = hwnd.0 as isize;
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut ctx = Ctx { pid, hwnd: 0 };
    unsafe {
        let _ = EnumWindows(
            Some(cb),
            LPARAM(&mut ctx as *mut Ctx as isize),
        );
        if ctx.hwnd != 0 {
            let _ = ShowWindow(HWND(ctx.hwnd as *mut _), SW_MINIMIZE);
        }
    }
}

fn parse_orders(body: &str, out: &mut Vec<(String, String)>) {
    if !(body.contains("app_store_item") || body.contains("orders_rmas")) {
        return;
    }
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mut stack = vec![v];
    while let Some(cur) = stack.pop() {
        match cur {
            serde_json::Value::Object(m) => {
                if let Some(item) = m.get("app_store_item") {
                    let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let name = item
                        .get("display_name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    if !id.is_empty() && !out.iter().any(|(i, _)| i == id) {
                        out.push((id.to_string(), name.to_string()));
                    }
                }
                for (_, val) in m {
                    stack.push(val);
                }
            }
            serde_json::Value::Array(a) => {
                for val in a {
                    stack.push(val);
                }
            }
            _ => {}
        }
    }
}

/// Espera login en pedidos, minimiza la ventana, autoscroll y captura (id, nombre).
pub fn capture_orders(port: u16, pid: u32) -> Result<Vec<(String, String)>, String> {
    let deadline = Instant::now() + Duration::from_secs(900);
    let mut ws = page_ws(port, Instant::now() + Duration::from_secs(60))?.0;
    let mut idc = 100u32;
    cdp_call(&mut ws, 10, "Network.enable", "{}")?;
    let nav = serde_json::json!({"url": ORDERS_URL}).to_string();
    cdp_call(&mut ws, 11, "Page.navigate", &nav)?;
    loop {
        if Instant::now() > deadline {
            return Err("sin login en pedidos".into());
        }
        let t = eval(&mut ws, &mut idc, "document.body.innerText.slice(0,2000)");
        if t.contains("N.º de pedido") || t.contains("Comprado") || t.contains("Order #") {
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    crate::meta_auth::note("pedidos", "lista visible, capturando");
    if pid != 0 {
        minimize_pid(pid);
    }
    let mut out: Vec<(String, String)> = vec![];
    let mut quiet = 0u32;
    for _ in 0..150 {
        if Instant::now() > deadline {
            break;
        }
        let before = out.len();
        eval(&mut ws, &mut idc, "window.scrollTo(0, document.body.scrollHeight)");
        let t0 = Instant::now();
        let mut wanted: Vec<String> = vec![];
        while t0.elapsed() < Duration::from_secs(4) {
            let text = match ws.recv_text() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let v: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let method = v.get("method").and_then(|x| x.as_str()).unwrap_or("");
            let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);
            if method == "Network.requestWillBeSent" {
                let url = params
                    .get("request")
                    .and_then(|r| r.get("url"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let rid = params.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
                if (url.contains("/a/fl") || url.contains("api/graphql")) && !url.contains("rsrc.php") {
                    wanted.push(rid.to_string());
                }
            } else if method == "Network.loadingFinished" {
                let rid = params.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
                if wanted.contains(&rid.to_string()) {
                    idc += 1;
                    let gid = idc;
                    let _ = ws.send_text(&format!(
                        r#"{{"id":{gid},"method":"Network.getResponseBody","params":{{"requestId":"{rid}"}}}}"#
                    ));
                }
            } else if v.get("id").is_some() {
                if let Some(body) = v
                    .get("result")
                    .and_then(|x| x.get("body"))
                    .and_then(|x| x.as_str())
                {
                    if body.len() > 500 {
                        parse_orders(body, &mut out);
                    }
                }
            }
        }
        if out.len() == before {
            quiet += 1;
        } else {
            quiet = 0;
            crate::meta_auth::note("pedidos", &format!("{} juegos", out.len()));
        }
        if quiet >= 5 {
            break;
        }
    }
    Ok(out)
}

pub fn orders_file() -> std::path::PathBuf {
    crate::store::app_dir().join("orders_link.json")
}

pub fn save_orders_file(items: &[(String, String)]) {
    let v: Vec<serde_json::Value> = items
        .iter()
        .map(|(id, name)| serde_json::json!({"id": id, "name": name}))
        .collect();
    if let Some(p) = orders_file().parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(
        orders_file(),
        serde_json::to_string(&v).unwrap_or_default(),
    )
    .ok();
}

pub fn load_orders_file() -> Vec<(String, String)> {
    let raw = std::fs::read(orders_file()).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
    v.as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| {
            let id = x.get("id")?.as_str()?.to_string();
            let name = x.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            Some((id, name))
        })
        .collect()
}

/// Captura tras el SSO (misma ventana). Best-effort: nunca falla el vinculado.
pub fn capture_after_sso(port: u16, pid: u32) {
    match capture_orders(port, pid) {
        Ok(items) if !items.is_empty() => {
            save_orders_file(&items);
            crate::meta_auth::note("pedidos", &format!("guardados {} de vinculado", items.len()));
        }
        Ok(_) => crate::meta_auth::note("pedidos", "captura vacía"),
        Err(e) => crate::meta_auth::note("pedidos", &format!("captura omitida: {e}")),
    }
}
/// Login web interactivo (fallback manual: botón Importar pedidos).
pub fn login_interactive() -> Result<Vec<(String, String)>, String> {
    let port = {
        let s = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        s.local_addr().map_err(|e| e.to_string())?.port()
    };
    let profile = std::env::temp_dir().join(format!("qd-web-{}", std::process::id()));
    std::fs::create_dir_all(&profile).ok();
    let exe = match crate::browser::detect() {
        crate::browser::DefaultBrowser::Chromium(exe) => exe,
        _ => crate::meta_auth::edge_path()?,
    };
    let mut child = crate::browser::launch_chromium(&exe, port, &profile, ORDERS_URL)
        .map_err(|e| format!("No se pudo abrir el navegador: {e}"))?;
    let r = capture_orders(port, child.id());
    let _ = child.kill();
    std::fs::remove_dir_all(&profile).ok();
    r
}
