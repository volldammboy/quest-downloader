//! Catálogo público de la Meta Quest Store (vía OculusDB + Store pública).

use serde_json::Value;
use std::time::Duration;

const OCULUSDB: &str = "https://oculusdb.rui2015.me";
const OCAPI: &str = "https://www.meta.com/ocapi/graphql";
const ARTWORK_DOC: &str = "6549406941839522";
const TIMEOUT: u64 = 25;

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) QD/1.0")
        .build()
        .expect("http client")
}

#[derive(Clone, Debug, Default)]
pub struct StoreApp {
    pub id: String,
    pub name: String,
    pub package: String,
    pub price: String,
    pub size: String,
    pub publisher: String,
    pub genres: String,
    pub store_url: String,
    pub image_url: String,
}

fn s(v: Option<&Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

pub fn search_store(query: &str, limit: usize) -> Result<Vec<StoreApp>, String> {
    let q: String = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("%20")
        .chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || "-_.~%".contains(c) {
                vec![c]
            } else {
                format!("%{:02X}", c as u8).chars().collect::<Vec<_>>()
            }
        })
        .into_iter()
        .collect();
    if q.trim().is_empty() {
        return Ok(vec![]);
    }
    let raw: Value = client()
        .get(format!("{OCULUSDB}/api/v1/search/{q}"))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let mut apps = vec![];
    for a in raw.as_array().cloned().unwrap_or_default().into_iter().take(limit) {
        let id = a.get("id").map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default();
        let canonical = s(a.get("canonicalName"));
        let store_url = if canonical.is_empty() {
            format!("https://www.meta.com/experiences/p/{id}")
        } else {
            format!("https://www.meta.com/experiences/{canonical}/{id}/")
        };
        let img = s(a.get("imageLink"));
        let image_url = if img.starts_with('/') { format!("{OCULUSDB}{img}") } else { img };
        let genres = a
            .get("genre_names")
            .and_then(|v| v.as_array())
            .map(|g| g.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        apps.push(StoreApp {
            id,
            name: a
                .get("displayName")
                .or_else(|| a.get("appName"))
                .and_then(|v| v.as_str())
                .map(|v| v.to_string())
                .unwrap_or_else(|| crate::tr(&crate::cur_lang(), "Sin nombre")),
            package: s(a.get("packageName")),
            price: {
                let p = s(a.get("priceFormatted"));
                if p.is_empty() {
                    crate::tr(&crate::cur_lang(), "Gratis")
                } else {
                    p
                }
            },
            size: {
                let z = s(a.get("requiredSpaceAdjustedFormatted"));
                if z.is_empty() { "?".into() } else { z }
            },
            publisher: s(a.get("publisher_name")),
            genres,
            store_url,
            image_url,
        });
    }
    Ok(apps)
}

pub fn artwork_list(app_id: &str) -> Vec<String> {
    let vars = serde_json::json!({ "itemId": app_id, "hmdType": "MONTEREY" }).to_string();
    let mut form = std::collections::HashMap::new();
    form.insert("doc_id", ARTWORK_DOC);
    form.insert("variables", vars.as_str());
    let item: Value = client()
        .post(OCAPI)
        .form(&form)
        .send()
        .ok()
        .and_then(|r| r.json().ok())
        .unwrap_or(Value::Null);
    let item = item.get("data").and_then(|d| d.get("item")).cloned().unwrap_or(Value::Null);
    if item.get("id").and_then(|v| v.as_str()).unwrap_or("") != app_id {
        return vec![];
    }
    let mut urls = vec![];
    for k in ["cover_landscape_image", "hero", "cover_square_image", "icon_image"] {
        if let Some(u) = item.get(k).and_then(|v| v.get("uri")).and_then(|v| v.as_str()) {
            if !urls.contains(&u.to_string()) {
                urls.push(u.to_string());
            }
        }
    }
    urls
}

pub fn resolve_cover(id: &str, art_url: &str, image_url: &str) -> Vec<String> {
    let mut urls = vec![];
    for u in [
        art_url.to_string(),
        format!("{OCULUSDB}/cdn/images/{id}"),
        image_url.to_string(),
    ] {
        if !u.is_empty() && !urls.contains(&u) {
            urls.push(u);
        }
    }
    urls.extend(artwork_list(id));
    urls
}

pub fn download_cover(url: &str) -> Result<Vec<u8>, String> {
    let resp = client().get(url).send().map_err(|e| e.to_string())?;
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !ct.contains("image") {
        return Err("no es imagen".into());
    }
    let data = resp.bytes().map_err(|e| e.to_string())?.to_vec();
    if data.len() < 2000 {
        return Err("vacía".into());
    }
    Ok(data)
}

pub fn save_json(path: &std::path::Path, app: &StoreApp) {
    let v = serde_json::json!({
        "id": app.id, "name": app.name, "package": app.package,
        "price": app.price, "size": app.size, "publisher": app.publisher,
        "genres": app.genres, "store_url": app.store_url,
    });
    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("."))).ok();
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default()).ok();
}
