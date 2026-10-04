//! Persistencia local (sqlite): ajustes, biblioteca y descargas.

use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::PathBuf;

pub fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn app_dir() -> PathBuf {
    base_dir().join("data")
}

pub fn download_dir() -> PathBuf {
    base_dir().join("Downloads")
}

/// Renombra `datos/`→`data/` y `Descargas/`→`Downloads/` conservando el contenido.
pub fn migrate_dirs() {
    for (old, new) in [("datos", "data"), ("Descargas", "Downloads")] {
        let o = base_dir().join(old);
        let n = base_dir().join(new);
        if o.exists() && !n.exists() {
            std::fs::rename(&o, &n).ok();
        }
    }
}

pub fn covers_dir() -> PathBuf {
    app_dir().join("covers_v2")
}

fn conn() -> rusqlite::Result<Connection> {
    std::fs::create_dir_all(app_dir()).ok();
    let cx = Connection::open(app_dir().join("data.db"))?;
    cx.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings(k TEXT PRIMARY KEY, v TEXT);
         CREATE TABLE IF NOT EXISTS library(
            id TEXT PRIMARY KEY, name TEXT, package TEXT, price TEXT,
            size TEXT, publisher TEXT, store_url TEXT, added INTEGER,
            owned INTEGER DEFAULT 0, verified_at INTEGER DEFAULT 0,
            data TEXT DEFAULT '', art_url TEXT DEFAULT '');",
    )?;
    let cols: Vec<String> = cx
        .prepare("PRAGMA table_info(library)")?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, ddl) in [
        ("owned", "ALTER TABLE library ADD COLUMN owned INTEGER DEFAULT 0"),
        ("verified_at", "ALTER TABLE library ADD COLUMN verified_at INTEGER DEFAULT 0"),
        ("data", "ALTER TABLE library ADD COLUMN data TEXT DEFAULT ''"),
        ("art_url", "ALTER TABLE library ADD COLUMN art_url TEXT DEFAULT ''"),
    ] {
        if !cols.iter().any(|c| c == name) {
            cx.execute_batch(ddl)?;
        }
    }
    Ok(cx)
}

pub fn get_setting(key: &str) -> String {
    conn()
        .and_then(|cx| {
            cx.query_row("SELECT v FROM settings WHERE k=?", [key], |r| {
                r.get::<_, String>(0)
            })
        })
        .unwrap_or_default()
}

pub fn set_setting(key: &str, value: &str) {
    if let Ok(cx) = conn() {
        cx.execute(
            "INSERT OR REPLACE INTO settings(k,v) VALUES(?,?)",
            params![key, value],
        )
        .ok();
    }
}

#[derive(Clone, Debug, Default)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub package: String,
    pub price: String,
    pub size: String,
    pub publisher: String,
    pub store_url: String,
    pub owned: bool,
    pub verified_at: i64,
    pub art_url: String,
    pub info: Value,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn library_add(game: &Game) {
    let Ok(cx) = conn() else { return };
    let prev: Option<(i64, i64, String)> = cx
        .query_row(
            "SELECT owned, verified_at, art_url FROM library WHERE id=?",
            [&game.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (owned, verified_at, mut art) =
        prev.unwrap_or((0, 0, String::new()));
    if !game.art_url.is_empty() {
        art = game.art_url.clone();
    }
    let blob = serde_json::to_string(&serde_json::json!({
        "id": game.id, "name": game.name, "package": game.package,
        "price": game.price, "size": game.size, "publisher": game.publisher,
        "store_url": game.store_url,
        "genres": game.info.get("genres").cloned().unwrap_or(Value::Null),
        "image_url": game.info.get("image_url").cloned().unwrap_or(Value::Null),
        "art_url": art,
    }))
    .unwrap_or_default();
    cx.execute(
        "INSERT OR REPLACE INTO library VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            game.id,
            game.name,
            game.package,
            game.price,
            game.size,
            game.publisher,
            game.store_url,
            now(),
            owned,
            verified_at,
            blob,
            art
        ],
    )
    .ok();
}

pub fn library_set_owned(app_id: &str, owned: bool) {
    if let Ok(cx) = conn() {
        cx.execute(
            "UPDATE library SET owned=?, verified_at=? WHERE id=?",
            params![owned as i64, now(), app_id],
        )
        .ok();
    }
}

pub fn library_set_art(app_id: &str, url: &str) {
    if let Ok(cx) = conn() {
        cx.execute(
            "UPDATE library SET art_url=? WHERE id=?",
            params![url, app_id],
        )
        .ok();
    }
}

pub fn library_all() -> Vec<Game> {
    let Ok(cx) = conn() else { return vec![] };
    let mut st = match cx.prepare(
        "SELECT id,name,package,price,size,publisher,store_url,owned,verified_at,data,art_url
         FROM library ORDER BY name",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    st.query_map([], |r| {
        let data: String = r.get(9)?;
        let info: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        Ok(Game {
            id: r.get(0)?,
            name: r.get(1)?,
            package: r.get(2)?,
            price: r.get(3)?,
            size: r.get(4)?,
            publisher: r.get(5)?,
            store_url: r.get(6)?,
            owned: r.get::<_, i64>(7)? == 1,
            verified_at: r.get(8)?,
            art_url: r.get(10)?,
            info,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn library_remove(app_id: &str) {
    if let Ok(cx) = conn() {
        cx.execute("DELETE FROM library WHERE id=?", [app_id]).ok();
    }
}
