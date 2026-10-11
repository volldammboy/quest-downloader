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
    match cdp_call(ws, *id, "Runtime.evaluate", &params) {
        Ok(v) => {
            if let Some(ex) = v.get("result").and_then(|x| x.get("exceptionDetails")) {
                let t = ex.get("text").and_then(|x| x.as_str()).unwrap_or("?");
                let line = ex.get("lineNumber").and_then(|x| x.as_u64()).unwrap_or(0);
                let col = ex.get("columnNumber").and_then(|x| x.as_u64()).unwrap_or(0);
                crate::meta_auth::note("pedidos", &format!("JS exc {t} @{line}:{col}"));
            }
            v.get("result")
                .and_then(|x| x.get("result"))
                .and_then(|x| x.get("value"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default()
        }
        Err(e) => {
            crate::meta_auth::note("pedidos", &format!("JS eval err: {e}"));
            String::new()
        }
    }
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

/// Worker con fetch virgen (el fetch de la página lo intercepta Meta).
/// Sin comillas dobles ni barras: se inyecta entre comillas sin escapar.
const WORKER_CODE: &str = "onmessage=async e=>{try{const r=await fetch('https://www.meta.com/api/graphql/',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},credentials:'include',body:e.data});postMessage('HTTP'+r.status+' '+(await r.text()))}catch(x){postMessage('JSERR:'+x)}}";
const RUN_JS: &str = "(async()=>{try{const w=new Worker(URL.createObjectURL(new Blob([window.__qd_code],{type:'text/javascript'})));const t=await new Promise((res,rej)=>{w.onmessage=m=>res(m.data);w.onerror=x=>rej(x);w.postMessage(window.__qd_b)});w.terminate();return String(t).slice(0,2000000)}catch(e){return 'JSERR:'+e}})()";

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
        None => {
            crate::meta_auth::note("pedidos", "fetch: sin variables en la consulta");
            return;
        }
    };
    let mut vars: serde_json::Value = match serde_json::from_str(&pairs[vi].1) {
        Ok(v) => v,
        Err(_) => {
            crate::meta_auth::note("pedidos", "fetch: variables rotas (postData truncado?)");
            return;
        }
    };
    if let Some(o) = vars.as_object_mut() {
        o.insert("first".into(), serde_json::json!(20));
    }
    let mut cursor: Option<String> = None;
    let mut seen_c = std::collections::HashSet::new();
    if let Some(o) = vars.as_object_mut() {
        let keys: Vec<String> = o.keys().cloned().collect();
        crate::meta_auth::note("pedidos", &format!("fetch vars: {}", keys.join(",")));
    }
    for page in 0..60 {
        if !seen_c.insert(cursor.clone()) {
            crate::meta_auth::note("pedidos", &format!("fetch pág {page}: cursor repetido"));
            break;
        }
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
        // El cuerpo ya va codificado: sin comillas ni barras, inyección segura.
        if page == 0 {
            eval(ws, idc, &["window.__qd_code=\"", WORKER_CODE, "\""].concat());
        }
        eval(ws, idc, &["window.__qd_b=\"", &body, "\""].concat());
        let text = eval_async(ws, idc, RUN_JS);
        if text.is_empty() {
            crate::meta_auth::note("pedidos", &format!("fetch pág {page}: vacío"));
            break;
        }
        if text.starts_with("JSERR") {
            crate::meta_auth::note("pedidos", &format!("fetch pág {page}: {}", text.chars().take(160).collect::<String>()));
            break;
        }
        let body = match text.find('{') {
            Some(i) => &text[i..],
            None => {
                crate::meta_auth::note("pedidos", &format!("fetch pág {page}: sin JSON"));
                break;
            }
        };
        let before = out.len();
        parse_orders(body, out);
        let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
        let ecount = v
            .pointer("/data/node/orders_rmas_and_subscriptions/edges")
            .and_then(|e| e.as_array())
            .map(|e| e.len())
            .unwrap_or(0);
        if page == 0 {
            let p = crate::store::app_dir().join("pedidos-fetch0.json");
            if let Some(pp) = p.parent() {
                std::fs::create_dir_all(pp).ok();
            }
            std::fs::write(&p, body).ok();
        }
        crate::meta_auth::note("pedidos", &format!("fetch pág {page}: {} juegos ({ecount} edges)", out.len()));
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
        if out.len() == before {
            crate::meta_auth::note("pedidos", &format!("fetch pág {page}: sin novedades"));
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

/// Ventana pequeña en la esquina (sin foco): debe seguir visible en pantalla,
/// si no el sistema suspende la paginación.
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
            let (x, y) = unsafe {
                let cx = GetSystemMetrics(SM_CXSCREEN);
                let cy = GetSystemMetrics(SM_CYSCREEN);
                (cx - 400, cy - 320)
            };
            let _ = SetWindowPos(
                HWND(ctx.hwnd as *mut _),
                HWND(std::ptr::null_mut()),
                x,
                y,
                360,
                240,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// Bundles vistos en pedidos (para expandir a títulos individuales).
static BUNDLES: std::sync::OnceLock<std::sync::Mutex<Vec<(String, String)>>> =
    std::sync::OnceLock::new();

fn save_bundle(id: &str, name: &str) {
    let mut b = BUNDLES
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    if !b.iter().any(|(i, _)| i == id) {
        b.push((id.to_string(), name.to_string()));
    }
}

pub fn take_bundles() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = BUNDLES
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .drain(..)
        .collect();
    // Persistidos de una captura anterior.
    let f = crate::store::app_dir().join("bundles.json");
    if let Ok(raw) = std::fs::read(&f) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
            if let Some(a) = v.as_array() {
                for x in a {
                    if let (Some(id), Some(name)) = (
                        x.get("id").and_then(|n| n.as_str()),
                        x.get("name").and_then(|n| n.as_str()),
                    ) {
                        if !out.iter().any(|(i, _)| i == id) {
                            out.push((id.to_string(), name.to_string()));
                        }
                    }
                }
            }
        }
        std::fs::remove_file(&f).ok();
    }
    out
}

fn save_bundles_file(items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }
    let v: Vec<serde_json::Value> = items
        .iter()
        .map(|(id, name)| serde_json::json!({"id": id, "name": name}))
        .collect();
    if let Some(p) = crate::store::app_dir().parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(
        crate::store::app_dir().join("bundles.json"),
        serde_json::to_string(&v).unwrap_or_default(),
    )
    .ok();
}

/// Un pedido = nodo con `app_store_item` + `status`.
/// Solo juegos completos (`XSHOCApplication`), sin cancelados ni DLCs.
/// Los bundles se guardan aparte para expandirlos.
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
                    let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let name = item
                        .get("display_name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    if id.is_empty() || skip {
                        // (sigue barriendo hijos)
                    } else if ty.contains("Bundle") || ty.contains("Pack") {
                        save_bundle(id, name);
                    } else if ty == "XSHOCApplication" {
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
    let mut sent: Vec<(u64, String, String)> = vec![]; // (id CDP, requestId, url)
    let mut dbg_n = 0u32;
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
            let m = params
                .get("request")
                .and_then(|r| r.get("method"))
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            if m == "POST" {
                let tail: String = url.chars().rev().take(110).collect::<String>().chars().rev().collect();
                crate::meta_auth::note("pedidos", &format!("POST {tail}"));
            }
            if (url.contains("/a/fl") || url.contains("api/graphql")) && !url.contains("rsrc.php") {
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
                            crate::meta_auth::note("pedidos", &format!("consulta POST len={}", pd.len()));
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
                let u = wanted.iter().find(|(id, _)| id == rid).map(|(_, u)| u.clone()).unwrap_or_default();
                sent.push((gid as u64, rid.to_string(), u));
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
                let gid = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                let u = sent.iter().find(|(g, _, _)| *g == gid).map(|(_, _, u)| u.clone()).unwrap_or_default();
                let tail: String = u.chars().rev().take(80).collect::<String>().chars().rev().collect();
                crate::meta_auth::note("pedidos", &format!("cuerpo len={} {tail}", body.len()));
                if dbg_n < 4 {
                    let p = crate::store::app_dir().join(format!("pedidos-dbg-{dbg_n}.txt"));
                    if let Some(pp) = p.parent() {
                        std::fs::create_dir_all(pp).ok();
                    }
                    std::fs::write(&p, format!("URL: {u}\n\n{body}")).ok();
                    dbg_n += 1;
                }
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

/// Extrae pedidos incrustados en el HTML (Meta los pinta sin pedirlos por red).
const DOM_JS: &str = r#"(()=>{const h=document.documentElement.innerHTML;const out=[];const re=/"status":"([A-Z_]+)"[\s\S]{0,3000}?"app_store_item":\{"__typename":"([^"]+)","id":"(\d+)","display_name":"((?:[^"\\]|\\.)*)"(?:[\s\S]{0,1500}?"uri":"((?:[^"\\]|\\.)*)")?/g;let m;let g=0;while((m=re.exec(h))&&g++<3000){out.push([m[1],m[2],m[3],m[4],m[5]||""]);}return JSON.stringify(out);})()"#;

fn dom_extract(
    ws: &mut crate::meta_auth::Ws,
    idc: &mut u32,
    out: &mut Vec<(String, String, String)>,
) {
    let t = eval(ws, idc, DOM_JS);
    if t.is_empty() {
        return;
    }
    let arr: Vec<Vec<String>> = serde_json::from_str(&t).unwrap_or_default();
    let mut n = 0u32;
    for r in arr {
        if r.len() < 4 {
            continue;
        }
        let (status, ty, id, name) = (&r[0], &r[1], &r[2], &r[3]);
        let cover = r.get(4).cloned().unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if matches!(status.as_str(), "CANCELED" | "REFUNDED" | "REVERSED" | "FAILED") {
            continue;
        }
        if ty.contains("Bundle") || ty.contains("Pack") {
            save_bundle(id, name);
            continue;
        }
        if ty != "XSHOCApplication" {
            continue;
        }
        if !out.iter().any(|(i, _, _)| i == id) {
            out.push((id.clone(), name.clone(), cover));
            n += 1;
        }
    }
    if n > 0 {
        crate::meta_auth::note("pedidos", &format!("{} juegos", out.len()));
    }
}

/// Emite el progreso si hay novedades (la UI va pintando la Biblioteca).
fn emit(
    out: &[(String, String, String)],
    sent: &mut usize,
    progress: &dyn Fn(Vec<(String, String, String)>),
) {
    if out.len() > *sent {
        *sent = out.len();
        progress(out.to_vec());
    }
}

/// Espera login en pedidos, oculta la ventana y captura (id, nombre, portada).
pub fn capture_orders(
    port: u16,
    pid: u32,
    progress: &dyn Fn(Vec<(String, String, String)>),
) -> Result<Vec<(String, String, String)>, String> {
    let deadline = Instant::now() + Duration::from_secs(900);
    let (mut ws, here) = page_ws(port, Instant::now() + Duration::from_secs(60))?;
    pending().clear();
    let mut idc = 100u32;
    cdp_call(&mut ws, 10, "Network.enable", "{}")?;
    // Sin caché: si no, las respuestas vienen 304/vacías y no hay cuerpos.
    match cdp_call(&mut ws, 12, "Network.setCacheDisabled", r#"{"cacheDisabled":true}"#) {
        Ok(_) => crate::meta_auth::note("pedidos", "caché CDP off"),
        Err(e) => crate::meta_auth::note("pedidos", &format!("caché CDP err: {e}")),
    }
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
    let mut sent = 0usize;
    let mut stat = (0u32, 0u32, 0u32);
    // Primero el DOM: la primera página viene incrustada en el HTML.
    dom_extract(&mut ws, &mut idc, &mut out);
    emit(&out, &mut sent, progress);
    // La carga fresca ya trae la primera página: recógela.
    drain(&mut ws, &mut idc, 10, &mut out, &mut stat);
    emit(&out, &mut sent, progress);
    let mut quiet = 0u32;
    for i in 0..150 {
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
        // El DOM crece con el scroll (por si la red no da cuerpos).
        if i % 3 == 2 {
            dom_extract(&mut ws, &mut idc, &mut out);
        }
        emit(&out, &mut sent, progress);
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
        emit(&out, &mut sent, progress);
    }
    // Última pasada al DOM por lo que haya quedado pintado.
    dom_extract(&mut ws, &mut idc, &mut out);
    emit(&out, &mut sent, progress);
    crate::meta_auth::note(
        "pedidos",
        &format!("resumen req={} fin={} cuerpos={} juegos={}", stat.0, stat.1, stat.2, out.len()),
    );
    // Guarda bundles para expandirlos aunque se cierre QD.
    save_bundles_file(&take_bundles());
    // (take los vacía de memoria; verify los relee del fichero)
    Ok(out)
}

/// Comprueba en headless si la sesión ya lista pedidos (sin ventana).
fn headless_listed(port: u16) -> bool {
    let mut ws = match page_ws(port, Instant::now() + Duration::from_secs(25)) {
        Ok((w, _)) => w,
        Err(_) => return false,
    };
    let mut idc = 10u32;
    let nav = serde_json::json!({"url": ORDERS_URL}).to_string();
    if cdp_call(&mut ws, 20, "Page.navigate", &nav).is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() > deadline {
            return false;
        }
        let t = eval(&mut ws, &mut idc, "document.body.innerText.slice(0,2000)");
        if t.contains("N.º de pedido") || t.contains("Comprado") || t.contains("Order #") {
            return true;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Login web interactivo (bypass manual del error 500).
pub fn login_interactive(
    progress: &dyn Fn(Vec<(String, String, String)>),
) -> Result<Vec<(String, String, String)>, String> {
    let port = {
        let s = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        s.local_addr().map_err(|e| e.to_string())?.port()
    };
    let profile = web_profile();
    std::fs::create_dir_all(&profile).ok();
    // Limpia cachés de disco (conserva cookies/sesión): cuerpos siempre frescos.
    for d in [
        "Default/Cache",
        "Default/Code Cache",
        "Default/GPUCache",
        "Default/Service Worker",
        "ShaderCache",
        "GrShaderCache",
    ] {
        std::fs::remove_dir_all(profile.join(d)).ok();
    }
    let exe = match crate::browser::detect() {
        crate::browser::DefaultBrowser::Chromium(exe) => exe,
        _ => crate::meta_auth::edge_path()?,
    };
    // 1) Invisible: si la sesión vale, sin ventana.
    if let Ok(mut headless) =
        crate::browser::launch_chromium_headless(&exe, port, &profile, ORDERS_URL)
    {
        if headless_listed(port) {
            crate::meta_auth::note("pedidos", "sesión válida, invisible");
            let r = capture_orders(port, 0, progress);
            let _ = headless.kill();
            std::thread::sleep(Duration::from_secs(1));
            return r;
        }
        let _ = headless.kill();
        std::thread::sleep(Duration::from_secs(2));
    }
    // 2) Visible para loguearse.
    let mut child = crate::browser::launch_chromium(&exe, port, &profile, ORDERS_URL)
        .map_err(|e| format!("No se pudo abrir el navegador: {e}"))?;
    let r = capture_orders(port, child.id(), progress);
    let _ = child.kill();
    std::thread::sleep(Duration::from_secs(1));
    r
}
