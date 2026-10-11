//! Sesión web de Meta (meta.com) para el historial de pedidos.
//!
//! La consulta `active_entitlements` (graph.oculus.com) devuelve 500 en
//! algunas cuentas. Alternativa: `DollyAccountOrder...Query` en
//! `meta.com/api/graphql` con sesión web (cookies + fb_dtsg).
//! El login es interactivo una vez; la sesión se guarda cifrada (DPAPI).

use std::time::{Duration, Instant};

const ORDERS_URL: &str =
    "https://www.meta.com/my/orders/?order_type=digital_products&timeFilter=ALL_TIME";
const GRAPHQL: &str = "https://www.meta.com/api/graphql/";
const ORDERS_DOC: &str = "37699748602973209";
const ORDERS_FRIENDLY: &str = "DollyAccountOrderAndRMAHistoryViewOrdersRmasAndSubscriptionsQuery";

#[derive(Clone, Debug, Default)]
pub struct WebCookie {
    pub name: String,
    pub value: String,
    pub exp: i64,
}

#[derive(Clone, Debug, Default)]
pub struct WebSession {
    pub cookies: Vec<WebCookie>,
    pub fb_dtsg: String,
    pub lsd: String,
    pub jazoest: String,
    pub c_user: String,
}

fn sess_path() -> std::path::PathBuf {
    crate::store::app_dir().join("web_session.bin")
}

pub fn has_session() -> bool {
    sess_path().exists()
}

pub fn forget() {
    std::fs::remove_file(sess_path()).ok();
}

pub fn save_session(s: &WebSession) -> Result<(), String> {
    let v = serde_json::json!({
        "cookies": s.cookies.iter().map(|c| serde_json::json!({"n": c.name, "v": c.value, "e": c.exp})).collect::<Vec<_>>(),
        "fb_dtsg": s.fb_dtsg, "lsd": s.lsd, "jazoest": s.jazoest, "c_user": s.c_user,
    });
    let raw = serde_json::to_vec(&v).map_err(|e| e.to_string())?;
    let enc = crate::meta_auth::dpapi(&raw, true)?;
    if let Some(p) = sess_path().parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(sess_path(), enc).map_err(|e| e.to_string())
}

pub fn load_session() -> Result<WebSession, String> {
    let enc = std::fs::read(sess_path()).map_err(|_| "Sin sesión web.".to_string())?;
    let raw = crate::meta_auth::dpapi(&enc, false)?;
    let v: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut s = WebSession::default();
    s.fb_dtsg = v.get("fb_dtsg").and_then(|x| x.as_str()).unwrap_or("").into();
    s.lsd = v.get("lsd").and_then(|x| x.as_str()).unwrap_or("").into();
    s.jazoest = v.get("jazoest").and_then(|x| x.as_str()).unwrap_or("").into();
    s.c_user = v.get("c_user").and_then(|x| x.as_str()).unwrap_or("").into();
    if let Some(cs) = v.get("cookies").and_then(|x| x.as_array()) {
        for c in cs {
            let n = c.get("n").and_then(|x| x.as_str()).unwrap_or("");
            let val = c.get("v").and_then(|x| x.as_str()).unwrap_or("");
            let exp = c.get("e").and_then(|x| x.as_i64()).unwrap_or(-1);
            if n.is_empty() || (exp > 0 && exp < now) {
                continue;
            }
            s.cookies.push(WebCookie { name: n.into(), value: val.into(), exp });
        }
    }
    if s.cookies.is_empty() || s.fb_dtsg.is_empty() {
        return Err("Sesión web caducada.".into());
    }
    Ok(s)
}

fn cookie_header(s: &WebSession) -> String {
    s.cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
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
                    for t in arr {
                        if t.get("type").and_then(|x| x.as_str()) != Some("page") {
                            continue;
                        }
                        let u = t.get("url").and_then(|x| x.as_str()).unwrap_or("");
                        let w = t
                            .get("webSocketDebuggerUrl")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        if w.is_empty() {
                            continue;
                        }
                        if u.contains("orders") || u.contains("meta.com/my") || u.contains("oculus.com") {
                            best = Some(w.into());
                            best_url = u.into();
                            break;
                        }
                        if best.is_none() {
                            best = Some(w.into());
                            best_url = u.into();
                        }
                    }
                    if let Some(w) = best {
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
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn scrape(html: &str, pats: &[&str]) -> String {
    for p in pats {
        // patrón simple: prefijo + captura hasta comilla
        if let Some(i) = html.find(p) {
            let rest = &html[i + p.len()..];
            let mut val = String::new();
            for c in rest.chars().take(300) {
                if c == '"' || c == '\'' || c == ',' || c == '}' || c == '&' {
                    break;
                }
                val.push(c);
            }
            let val = val.trim().trim_start_matches(|c| c == ':' || c == '=').trim();
            if !val.is_empty() && val.len() < 200 {
                return val.to_string();
            }
        }
    }
    String::new()
}

/// Intento silencioso tras el SSO: con el navegador aún abierto (usuario ya
/// logueado), abre pedidos y guarda la sesión web. Nunca falla el vinculado.
pub fn capture_after_sso(port: u16) {
    let res: Result<(), String> = (|| {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;
        // Espera pestaña.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut path = String::new();
        while path.is_empty() && Instant::now() < deadline {
            if let Ok(r) = http.get(format!("http://127.0.0.1:{port}/json/list")).send() {
                if let Ok(tabs) = r.json::<serde_json::Value>() {
                    if let Some(arr) = tabs.as_array() {
                        for t in arr {
                            if t.get("type").and_then(|x| x.as_str()) != Some("page") {
                                continue;
                            }
                            if let Some(u) = t.get("webSocketDebuggerUrl").and_then(|x| x.as_str()) {
                                if let Some(p) = u.split(&format!(":{port}")).nth(1) {
                                    path = p.to_string();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if path.is_empty() {
            return Err("sin pestaña".into());
        }
        let mut ws = crate::meta_auth::Ws::connect("127.0.0.1", port, &path)?;
        ws.stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        // Misma ventana del SSO: abre pedidos para completar la sesión web.
        // (El puente silencioso OIDC no cuaja; login manual una vez.)
        let nav = serde_json::json!({"url": "https://www.meta.com/my/orders/?order_type=digital_products&timeFilter=ALL_TIME"}).to_string();
        cdp_call(&mut ws, 20, "Page.navigate", &nav)?;
        // Espera larga: el usuario completa el login web en esa ventana.
        let deadline = Instant::now() + Duration::from_secs(600);
        // Bucle: espera a que el usuario complete el login web en esa ventana.
        crate::meta_auth::note("websess", "esperando login web en la ventana");
        let mut call_id = 22u32;
        loop {
            if Instant::now() > deadline {
                return Err("sin login web".into());
            }
            call_id += 1;
            let r = cdp_call(&mut ws, call_id, "Network.getAllCookies", "{}")?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mut sess = WebSession::default();
            let mut raw_cookies = vec![];
            if let Some(arr) = r.get("result").and_then(|x| x.get("cookies")).and_then(|x| x.as_array()) {
                for c in arr {
                    let domain = c.get("domain").and_then(|x| x.as_str()).unwrap_or("");
                    if !(domain.contains("meta.com") || domain.contains("facebook.com") || domain.contains("oculus.com")) {
                        continue;
                    }
                    let n = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let val = c.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let exp = c.get("expires").and_then(|x| x.as_i64()).unwrap_or(-1);
                    if n.is_empty() || val.is_empty() {
                        continue;
                    }
                    if n == "c_user" {
                        sess.c_user = val.into();
                    }
                    raw_cookies.push((n.to_string(), val.to_string(), exp));
                }
            }
            if sess.c_user.is_empty() || sess.c_user == "0" {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            call_id += 1;
            let r = cdp_call(
                &mut ws,
                call_id,
                "Runtime.evaluate",
                r#"{"expression":"document.documentElement.innerHTML.slice(0,3000000)","returnByValue":true}"#,
            )?;
            let html = r
                .get("result")
                .and_then(|x| x.get("result"))
                .and_then(|x| x.get("value"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            sess.fb_dtsg = scrape(html, &["\"fb_dtsg\":\"", "\"fbdtsg\":\"", "fb_dtsg="]);
            sess.lsd = scrape(html, &["\"lsd\":\""]);
            sess.jazoest = scrape(html, &["\"jazoest\":\"", "jazoest="]);
            if sess.fb_dtsg.is_empty() {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            for (n, val, exp) in raw_cookies {
                if exp > 0 && exp < now {
                    continue;
                }
                sess.cookies.push(WebCookie { name: n, value: val, exp });
            }
            save_session(&sess)?;
            crate::meta_auth::note("websess", "sesión web capturada en vinculado");
            return Ok(());
        }
    })();
    if let Err(e) = res {
        crate::meta_auth::note("websess", &format!("captura omitida: {e}"));
    }
}
pub fn login_interactive() -> Result<WebSession, String> {
    let port = {
        let s = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        s.local_addr().map_err(|e| e.to_string())?.port()
    };
    let profile = std::env::temp_dir().join(format!("qd-web-{}", std::process::id()));
    std::fs::create_dir_all(&profile).ok();
    // Mismo criterio que Vincular: predeterminado Chromium, si no Edge.
    let exe = match crate::browser::detect() {
        crate::browser::DefaultBrowser::Chromium(exe) => {
            crate::meta_auth::note("websess", &format!("navegador: {exe}"));
            exe
        }
        _ => crate::meta_auth::edge_path()?,
    };
    let mut child =
        crate::browser::launch_chromium(&exe, port, &profile, ORDERS_URL)
            .map_err(|e| format!("No se pudo abrir el navegador: {e}"))?;
    crate::meta_auth::note("websess", &format!("ventana pid={}", child.id()));
    let deadline = Instant::now() + Duration::from_secs(600);
    let (mut ws, _) = page_ws(port, deadline)?;
    // Espera a c_user (login completo).
    let mut sess = WebSession::default();
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            std::fs::remove_dir_all(&profile).ok();
            return Err("Tiempo agotado esperando el login.".into());
        }
        let r = cdp_call(&mut ws, 10, "Network.getAllCookies", "{}").unwrap_or(serde_json::Value::Null);
        let mut cookies = vec![];
        if let Some(arr) = r.get("result").and_then(|x| x.get("cookies")).and_then(|x| x.as_array()) {
            for c in arr {
                let domain = c.get("domain").and_then(|x| x.as_str()).unwrap_or("");
                if !(domain.contains("meta.com") || domain.contains("facebook.com") || domain.contains("oculus.com")) {
                    continue;
                }
                let n = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let val = c.get("value").and_then(|x| x.as_str()).unwrap_or("");
                let exp = c.get("expires").and_then(|x| x.as_i64()).unwrap_or(-1);
                if n.is_empty() {
                    continue;
                }
                if n == "c_user" {
                    sess.c_user = val.into();
                }
                cookies.push((n.to_string(), val.to_string(), exp));
            }
        }
        if !sess.c_user.is_empty() && sess.c_user != "0" {
            crate::meta_auth::note("websess", "login detectado, leyendo sesión");
            let r = cdp_call(
                &mut ws,
                11,
                "Runtime.evaluate",
                r#"{"expression":"document.documentElement.innerHTML.slice(0,3000000)","returnByValue":true}"#,
            )
            .unwrap_or(serde_json::Value::Null);
            let html = r
                .get("result")
                .and_then(|x| x.get("result"))
                .and_then(|x| x.get("value"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            sess.fb_dtsg = scrape(html, &["\"fb_dtsg\":\"", "\"fbdtsg\":\"", "fb_dtsg="]);
            sess.lsd = scrape(html, &["\"lsd\":\""]);
            sess.jazoest = scrape(html, &["\"jazoest\":\"", "jazoest="]);
            if !sess.fb_dtsg.is_empty() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                for (n, val, exp) in cookies {
                    if exp > 0 && exp < now {
                        continue;
                    }
                    sess.cookies.push(WebCookie { name: n, value: val, exp });
                }
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    let _ = child.kill();
    std::thread::sleep(Duration::from_secs(1));
    std::fs::remove_dir_all(&profile).ok();
    save_session(&sess)?;
    crate::meta_auth::note("websess", "sesión web guardada");
    Ok(sess)
}

fn get_or_login() -> Result<WebSession, String> {
    match load_session() {
        Ok(s) => Ok(s),
        Err(_) => login_interactive(),
    }
}

fn gql(sess: &WebSession, vars: &serde_json::Value, req_n: u64) -> Result<serde_json::Value, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let vars_s = serde_json::to_string(vars).map_err(|e| e.to_string())?;
    let mut form: Vec<(String, String)> = vec![
        ("av".into(), sess.c_user.clone()),
        ("__user".into(), sess.c_user.clone()),
        ("__a".into(), "1".into()),
        ("__req".into(), format!("{req_n}")),
        ("dpr".into(), "1".into()),
        ("__ccg".into(), "EXCELLENT".into()),
        ("__rev".into(), "1049974625".into()),
        ("__comet_req".into(), "18".into()),
        ("fb_dtsg".into(), sess.fb_dtsg.clone()),
        ("lsd".into(), sess.lsd.clone()),
        ("__spin_r".into(), "1049974625".into()),
        ("__spin_b".into(), "trunk".into()),
        ("__spin_t".into(), format!("{now}")),
        ("fb_api_caller_class".into(), "RelayModern".into()),
        ("fb_api_req_friendly_name".into(), ORDERS_FRIENDLY.into()),
        ("server_timestamps".into(), "true".into()),
        ("variables".into(), vars_s),
        ("doc_id".into(), ORDERS_DOC.into()),
    ];
    if !sess.jazoest.is_empty() {
        form.push(("jazoest".into(), sess.jazoest.clone()));
    }
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36")
        .build()
        .map_err(|e| e.to_string())?
        .post(GRAPHQL)
        .header("Cookie", cookie_header(sess))
        .header("Origin", "https://www.meta.com")
        .header("Referer", ORDERS_URL)
        .form(&pairs)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<serde_json::Value>().map_err(|e| e.to_string())
}

/// Importa pedidos (compras digitales): devuelve (id, nombre) con paginación.
pub fn fetch_owned_apps() -> Result<Vec<(String, String)>, String> {
    let sess = get_or_login()?;
    // id de comercio: primero prueba con c_user; si falla, bootstrap.
    let mut out = vec![];
    let mut req_n = 1u64;
    // Intento 1: id = c_user.
    // Intento 2: si vacío, rasca el id de xsh_me desde la página de pedidos.
    let mut commerce_id = sess.c_user.clone();
    for attempt in 0..2 {
        out.clear();
        let mut cursor: Option<String> = None;
        loop {
            let vars = serde_json::json!({
                "cursor": cursor,
                "end_timestamp_in_seconds": serde_json::Value::Null,
                "first": 20,
                "funding_source_filter": serde_json::Value::Null,
                "order_types": ["DIGITAL_ORDERS"],
                "start_timestamp_in_seconds": serde_json::Value::Null,
                "id": commerce_id,
                "__relay_internal__pv__DollyInlineInvoiceCTArelayprovider": false,
            });
            let v = gql(&sess, &vars, req_n)?;
            req_n += 1;
            if let Some(errs) = v.get("errors") {
                let msg = serde_json::to_string(errs).unwrap_or_default();
                crate::meta_auth::note("pedidos", &format!("GraphQL errors: {}", &msg.chars().take(200).collect::<String>()));
                if attempt == 0 {
                    break;
                }
                return Err("Meta rechazó la consulta de pedidos.".into());
            }
            let conn = v
                .get("data")
                .and_then(|d| d.get("node"))
                .and_then(|n| n.get("orders_rmas_and_subscriptions"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let edges = conn.get("edges").and_then(|e| e.as_array()).cloned().unwrap_or_default();
            if edges.is_empty() && out.is_empty() && attempt == 0 {
                break; // probar con otro id
            }
            for e in &edges {
                let node = e.get("node").cloned().unwrap_or(serde_json::Value::Null);
                let item = node.get("app_store_item").cloned().unwrap_or(serde_json::Value::Null);
                let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
                let name = item.get("display_name").and_then(|x| x.as_str()).unwrap_or("");
                if !id.is_empty() {
                    out.push((id.to_string(), name.to_string()));
                }
            }
            let pi = conn.get("pageInfo").cloned().unwrap_or(serde_json::Value::Null);
            let has_next = pi.get("has_next_page").and_then(|x| x.as_bool()).unwrap_or(false);
            cursor = pi.get("end_cursor").and_then(|x| x.as_str()).map(|s| s.to_string());
            if !has_next || cursor.is_none() || out.len() >= 2000 {
                break;
            }
        }
        if !out.is_empty() || attempt == 1 {
            break;
        }
        // Intento 2: id de comercio desde la página.
        commerce_id = bootstrap_commerce_id(&sess).unwrap_or_default();
        if commerce_id.is_empty() {
            break;
        }
    }
    if out.is_empty() {
        forget();
        return Err("Sin pedidos: reenlaza la sesión web.".into());
    }
    crate::meta_auth::note("pedidos", &format!("{} juegos", out.len()));
    Ok(out)
}

fn bootstrap_commerce_id(sess: &WebSession) -> Result<String, String> {
    let html = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36")
        .build()
        .map_err(|e| e.to_string())?
        .get(ORDERS_URL)
        .header("Cookie", cookie_header(sess))
        .send()
        .map_err(|e| e.to_string())?
        .text()
        .map_err(|e| e.to_string())?;
    for pat in ["\"shop_cart\":null,\"id\":\"", "xsh_me\":{\"shop_cart\":null,\"id\":\""] {
        if let Some(i) = html.find(pat) {
            let rest = &html[i + pat.len()..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if id.len() > 10 {
                return Ok(id);
            }
        }
    }
    Err("Sin id de comercio.".into())
}
