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

/// Perfil persistente del navegador temporal: Meta reconoce el dispositivo y
/// no pide código en cada intento. Vive en `data/` (no se sube a GitHub).
pub fn web_profile() -> std::path::PathBuf {
    crate::store::app_dir().join("web-profile")
}

/// Eventos CDP guardados mientras `cdp_call` espera su respuesta (los eval
/// los descartaban: ahí se perdían cuerpos). `drain` los consume.
static PENDING: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
    std::sync::OnceLock::new();

fn pending() -> std::sync::MutexGuard<'static, Vec<String>> {
    PENDING
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap()
}

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
        pending().push(text);
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

fn eval_async(ws: &mut crate::meta_auth::Ws, id: &mut u32, expr: &str) -> String {
    *id += 1;
    let params = serde_json::json!({"expression": expr, "awaitPromise": true, "returnByValue": true}).to_string();
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

fn pct_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexv(b[i + 1]), hexv(b[i + 2])) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(' ');
        } else {
            out.push(b[i] as char);
        }
        i += 1;
    }
    out
}

fn hexv(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Pagina la consulta de pedidos con fetch en la propia página (su sesión ya
/// vale): completo en segundos, sin depender del scroll.
fn fetch_pages(
    ws: &mut crate::meta_auth::Ws,
    idc: &mut u32,
    post: &str,
    out: &mut Vec<(String, String, String)>,
) {
    let mut pairs: Vec<(String, String)> = vec![];
    for kv in post.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            pairs.push((pct_decode(k), pct_decode(v)));
        }
    }
    let vi = match pairs.iter().position(|(k, _)| k == "variables") {
        Some(i) => i,
        None => return,
    };
    let mut vars: serde_json::Value = match serde_json::from_str(&pairs[vi].1) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(o) = vars.as_object_mut() {
        o.insert("first".into(), serde_json::json!(20));
    }
    let mut cursor: Option<String> = None;
    for page in 0..60 {
        if let Some(o) = vars.as_object_mut() {
            o.insert(
                "cursor".into(),
                cursor.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
            );
        }
        let body = pairs
            .iter()
            .map(|(k, v)| {
                let val = if k == "variables" {
                    serde_json::to_string(&vars).unwrap_or_default()
                } else {
                    v.clone()
                };
                format!("{}={}", pct_encode(k), pct_encode(&val))
            })
            .collect::<Vec<_>>()
            .join("&");
        let js_body = serde_json::to_string(&body).unwrap_or_default();
        let expr = format!(
            "(async () => {{ const r = await fetch('https://www.meta.com/api/graphql/', {{method:'POST', headers:{{'Content-Type':'application/x-www-form-urlencoded'}}, credentials:'include', body: {js_body}}}); return (await r.text()).slice(0, 2000000); }})()"
        );
        let text = eval_async(ws, idc, &expr);
        if text.is_empty() {
            crate::meta_auth::note("pedidos", &format!("fetch pág {page}: vacío"));
            break;
        }
        let before = out.len();
        parse_orders(&text, out);
        crate::meta_auth::note("pedidos", &format!("fetch pág {page}: {} juegos", out.len()));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let edges = v
            .pointer("/data/node/orders_rmas_and_subscriptions/edges")
            .and_then(|e| e.as_array());
        let mut next: Option<String> = None;
        if let Some(edges) = edges {
            if edges.is_empty() {
                break;
            }
            next = edges
                .last()
                .and_then(|e| e.get("cursor"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
        }
        if out.len() == before && page > 0 {
            break;
        }
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
        if out.len() >= 2000 {
            break;
        }
    }
}

/// Ventana fuera de la pantalla (oculta pero visible para el sistema):
/// la paginación sigue disparando sin molestar.
fn hide_pid(pid: u32) {
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
            let _ = SetWindowPos(
                HWND(ctx.hwnd as *mut _),
                HWND(std::ptr::null_mut()),
                -10000,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// Un pedido = nodo con `app_store_item` + `status`.
/// Solo juegos completos (`XSHOCApplication`), sin cancelados ni DLCs/bundles.
fn parse_orders(body: &str, out: &mut Vec<(String, String, String)>) {
    if !body.contains("app_store_item") {
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
                if let (Some(item), Some(status)) =
                    (m.get("app_store_item"), m.get("status").and_then(|x| x.as_str()))
                {
                    let skip = matches!(status, "CANCELED" | "REFUNDED" | "REVERSED" | "FAILED");
                    let ty = item.get("__typename").and_then(|x| x.as_str()).unwrap_or("");
                    if !skip && ty == "XSHOCApplication" {
                        let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
                        let name = item
                            .get("display_name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let cover = item
                            .get("icon_image")
                            .and_then(|x| x.get("uri"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        if !id.is_empty() && !out.iter().any(|(i, _, _)| i == id) {
                            out.push((id.to_string(), name.to_string(), cover.to_string()));
                        }
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

/// postData de la consulta de pedidos (para paginarla por fetch en página).
static QUERY: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

fn save_query(s: &str) {
    let mut q = QUERY
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap();
    if q.is_none() {
        *q = Some(s.to_string());
    }
}

fn take_query() -> Option<String> {
    QUERY
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap()
        .take()
}

/// Recoge respuestas de pedidos durante `secs` segundos.
fn drain(ws: &mut crate::meta_auth::Ws, idc: &mut u32, secs: u64, out: &mut Vec<(String, String, String)>, stat: &mut (u32, u32, u32)) {
    let t0 = Instant::now();
    let mut wanted: Vec<(String, String)> = vec![]; // (requestId, método+url)
    loop {
        if t0.elapsed() >= Duration::from_secs(secs) {
            break;
        }
        let text = if let Some(t) = pending().pop() {
            t
        } else {
            match ws.recv_text() {
                Ok(t) => t,
                Err(_) => continue,
            }
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
                let m = params
                    .get("request")
                    .and_then(|r| r.get("method"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("?");
                wanted.push((rid.to_string(), format!("{m} {url}")));
                stat.0 += 1;
                // Guarda el cuerpo de la consulta de datos para paginarla luego.
                if url.contains("api/graphql") {
                    if let Some(pd) = params
                        .get("request")
                        .and_then(|r| r.get("postData"))
                        .and_then(|x| x.as_str())
                    {
                        if pd.contains("SubscriptionsQuery") {
                            save_query(pd);
                        }
                    }
                }
            }
        } else if method == "Network.responseReceived" {
            let rid = params.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
            if let Some((_, u)) = wanted.iter().find(|(id, _)| id == rid) {
                let resp = params.get("response").cloned().unwrap_or(serde_json::Value::Null);
                let status = resp.get("status").and_then(|x| x.as_u64()).unwrap_or(0);
                let cached = resp.get("fromDiskCache").and_then(|x| x.as_bool()).unwrap_or(false);
                let tail: String = u.chars().rev().take(100).collect::<String>().chars().rev().collect();
                crate::meta_auth::note("pedidos", &format!("rr {status} cache={cached} {tail}"));
            }
        } else if method == "Network.loadingFinished" {
            let rid = params.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
            if wanted.iter().any(|(id, _)| id == rid) {
                stat.1 += 1;
                *idc += 1;
                let gid = *idc;
                let _ = ws.send_text(&format!(
                    r#"{{"id":{gid},"method":"Network.getResponseBody","params":{{"requestId":"{rid}"}}}}"#
                ));
            }
        } else if v.get("id").is_some() {
            if let Some(err) = v.get("error") {
                crate::meta_auth::note("pedidos", &format!("cuerpo error: {err}"));
            } else if let Some(body) = v
                .get("result")
                .and_then(|x| x.get("body"))
                .and_then(|x| x.as_str())
            {
                crate::meta_auth::note("pedidos", &format!("cuerpo len={}", body.len()));
                if body.len() > 500 {
                    stat.2 += 1;
                    // Muestra para afinar el filtro de DLCs (una vez por arranque).
                    let sample = crate::store::app_dir().join("pedidos-sample.json");
                    if !sample.exists() && body.contains("app_store_item") {
                        if let Some(p) = sample.parent() {
                            std::fs::create_dir_all(p).ok();
                        }
                        std::fs::write(&sample, body).ok();
                    }
                    let before = out.len();
                    parse_orders(body, out);
                    if out.len() != before {
                        crate::meta_auth::note("pedidos", &format!("{} juegos", out.len()));
                    }
                }
            }
        }
    }
}

/// Espera login en pedidos, oculta la ventana y captura (id, nombre, portada).
pub fn capture_orders(port: u16, pid: u32) -> Result<Vec<(String, String, String)>, String> {
    let deadline = Instant::now() + Duration::from_secs(900);
    let (mut ws, here) = page_ws(port, Instant::now() + Duration::from_secs(60))?;
    pending().clear();
    let mut idc = 100u32;
    cdp_call(&mut ws, 10, "Network.enable", "{}")?;
    // Sin caché: si no, las respuestas vienen 304/vacías y no hay cuerpos.
    let _ = cdp_call(&mut ws, 12, "Network.setCacheDisabled", r#"{"cacheDisabled":true}"#);
    crate::meta_auth::note("pedidos", &format!("pestaña {here}"));
    // Siempre carga fresca: la pestaña restaurada enseña caché sin red.
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
        hide_pid(pid);
    }
    let mut out: Vec<(String, String, String)> = vec![];
    let mut stat = (0u32, 0u32, 0u32);
    // La carga fresca ya trae la primera página: recógela.
    drain(&mut ws, &mut idc, 10, &mut out, &mut stat);
    let mut quiet = 0u32;
    for _ in 0..150 {
        if Instant::now() > deadline {
            break;
        }
        let before = out.len();
        // Scroll por pasos (como un humano): dispara la paginación mejor.
        for k in 1..=5 {
            eval(
                &mut ws,
                &mut idc,
                &format!("window.scrollTo(0, document.body.scrollHeight*{k}/5)"),
            );
            std::thread::sleep(Duration::from_millis(700));
        }
        drain(&mut ws, &mut idc, 4, &mut out, &mut stat);
        if out.len() == before {
            quiet += 1;
        } else {
            quiet = 0;
        }
        if quiet >= 5 {
            break;
        }
    }
    // Remate por fetch en página: trae TODO lo que falte, sin scroll.
    if let Some(post) = take_query() {
        crate::meta_auth::note("pedidos", &format!("paginando por fetch ({} enviados)", post.len()));
        fetch_pages(&mut ws, &mut idc, &post, &mut out);
    }
    crate::meta_auth::note(
        "pedidos",
        &format!("resumen req={} fin={} cuerpos={} juegos={}", stat.0, stat.1, stat.2, out.len()),
    );
    Ok(out)
}

/// Login web interactivo (bypass manual del error 500).
pub fn login_interactive() -> Result<Vec<(String, String, String)>, String> {
    let port = {
        let s = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        s.local_addr().map_err(|e| e.to_string())?.port()
    };
    let profile = web_profile();
    std::fs::create_dir_all(&profile).ok();
    let exe = match crate::browser::detect() {
        crate::browser::DefaultBrowser::Chromium(exe) => exe,
        _ => crate::meta_auth::edge_path()?,
    };
    let mut child = crate::browser::launch_chromium(&exe, port, &profile, ORDERS_URL)
        .map_err(|e| format!("No se pudo abrir el navegador: {e}"))?;
    let r = capture_orders(port, child.id());
    let _ = child.kill();
    std::thread::sleep(Duration::from_secs(1));
    r
}
