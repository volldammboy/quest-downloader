//! QD (Quest Downloader) — gestor de juegos de Meta Quest (Rust + egui).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod downloader;
mod meta_auth;
mod oculusdb;
mod store;

use downloader::{DownloadManager, JobStatus};
use eframe::egui;
use std::collections::HashMap;
use std::sync::{mpsc, Arc};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Lib,
    Store,
    Dl,
}

struct Cover {
    tex: egui::TextureHandle,
    w: f32,
    h: f32,
}

struct PlanDlg {
    app: store::Game,
    base: Vec<meta_auth::PlanFile>,
    langs: Vec<(bool, String, Vec<meta_auth::PlanFile>)>,
}

enum AsyncMsg {
    Search(Vec<oculusdb::StoreApp>),
    Sync(String, String, Vec<meta_auth::LibGame>),
    Plan(String, Result<meta_auth::Plan, String>),
    Cover(String, Vec<u8>),
    Linked(Result<String, String>),
}

struct App {
    page: Page,
    dm: Arc<DownloadManager>,
    library: Vec<store::Game>,
    covers: HashMap<String, Cover>,
    filter: String,
    selected: Option<String>,
    query: String,
    results: Vec<oculusdb::StoreApp>,
    searching: bool,
    syncing: bool,
    linking: bool,
    rx: mpsc::Receiver<AsyncMsg>,
    tx: mpsc::Sender<AsyncMsg>,
    plan_dlg: Option<PlanDlg>,
    plan_app: Option<store::Game>,
    plan_pending: bool,
    import_open: bool,
    import_text: String,
    confirm_del: bool,
    game_jobs: HashMap<String, Vec<String>>,
    account: String,
    dl_tex: Option<egui::TextureHandle>,
    web_tex: Option<egui::TextureHandle>,
    hits: Vec<(String, egui::Rect, egui::Rect, egui::Rect)>,
    press_down: bool,
    press_id: Option<String>,
    ncols: usize,
    lang: String,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        store::migrate_dirs();
        setup_fonts(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let app = Self {
            page: Page::Lib,
            dm: Arc::new(DownloadManager::new()),
            library: store::library_all(),
            covers: HashMap::new(),
            filter: String::new(),
            selected: None,
            query: String::new(),
            results: vec![],
            searching: false,
            syncing: false,
            linking: false,
            rx,
            tx,
            plan_dlg: None,
            plan_app: None,
            plan_pending: false,
            import_open: false,
            import_text: String::new(),
            confirm_del: false,
            game_jobs: HashMap::new(),
            account: store::get_setting("meta_name"),
            dl_tex: load_ui_icon(
                &cc.egui_ctx,
                include_bytes!("../icons/dl.png"),
                0.0,
                "icon-dl",
            ),
            web_tex: load_ui_icon(
                &cc.egui_ctx,
                include_bytes!("../icons/web.png"),
                0.0,
                "icon-web",
            ),
            hits: Vec::new(),
            press_down: false,
            press_id: None,
            ncols: 4,
            lang: eff_lang(),
        };
        set_cur_lang(&app.lang);
        app.ensure_covers();
        app
    }

    fn refresh_library(&mut self) {
        self.library = store::library_all();
    }

    fn selected_game(&self) -> Option<store::Game> {
        self.selected
            .as_ref()
            .and_then(|id| self.library.iter().find(|g| &g.id == id).cloned())
    }

    fn ensure_covers(&self) {
        let tx = self.tx.clone();
        let games = store::library_all();
        std::thread::spawn(move || {
            for g in games {
                let dest = store::covers_dir().join(format!("{}.jpg", g.id));
                if dest.exists() {
                    continue;
                }
                let mut urls = vec![];
                if !g.art_url.is_empty() {
                    urls.push(g.art_url.clone());
                }
                urls.push(format!(
                    "https://oculusdb.rui2015.me/cdn/images/{}",
                    g.id
                ));
                if let Some(img) = g.info.get("image_url").and_then(|v| v.as_str()) {
                    if !img.is_empty() {
                        urls.push(img.to_string());
                    }
                }
                urls.extend(oculusdb::artwork_list(&g.id));
                for u in urls {
                    match oculusdb::download_cover(&u) {
                        Ok(bytes) => {
                            std::fs::create_dir_all(store::covers_dir()).ok();
                            std::fs::write(&dest, &bytes).ok();
                            tx.send(AsyncMsg::Cover(g.id.clone(), bytes)).ok();
                            break;
                        }
                        Err(_) => continue,
                    }
                }
            }
        });
    }

    fn load_cover(&mut self, ctx: &egui::Context, id: &str, bytes: &[u8]) {
        if self.covers.contains_key(id) {
            return;
        }
        let Ok(img) = image::load_from_memory(bytes) else {
            return;
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        let color =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let tex = ctx.load_texture(
            format!("cover-{id}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.covers.insert(
            id.to_string(),
            Cover { tex, w: w as f32, h: h as f32 },
        );
    }

    fn do_search(&mut self) {
        let q = self.query.trim().to_string();
        if q.is_empty() || self.searching {
            return;
        }
        self.searching = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = oculusdb::search_store(&q, 20).unwrap_or_default();
            tx.send(AsyncMsg::Search(r)).ok();
        });
    }

    fn do_sync(&mut self) {
        if !meta_auth::is_linked() || self.syncing {
            return;
        }
        self.syncing = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let out = (|| -> Result<(String, String, Vec<meta_auth::LibGame>), String> {
                let token = meta_auth::profile_token()?;
                meta_auth::fetch_library(&token)
            })();
            match out {
                Ok((alias, uid, games)) => {
                    tx.send(AsyncMsg::Sync(alias, uid, games)).ok();
                }
                Err(e) => {
                    // reutiliza el canal Sync con error codificado en alias vacío
                    tx.send(AsyncMsg::Sync(String::new(), format!("ERR:{e}"), vec![]))
                        .ok();
                }
            }
        });
    }

    fn apply_sync(&mut self, alias: String, uid: String, games: Vec<meta_auth::LibGame>) {
        if uid.starts_with("ERR:") {
            self.syncing = false;
            return;
        }
        if !alias.is_empty() {
            store::set_setting("meta_name", &alias);
            self.account = alias;
        }
        if !uid.is_empty() {
            store::set_setting("meta_user_id", &uid);
        }
        let mut seen = std::collections::HashSet::new();
        for g in &games {
            let game = store::Game {
                id: g.id.clone(),
                name: if g.name.is_empty() { "?".into() } else { g.name.clone() },
                package: g.package.clone(),
                price: String::new(),
                size: String::new(),
                publisher: g.publisher.clone(),
                store_url: format!("https://www.meta.com/experiences/p/{}", g.id),
                owned: false,
                verified_at: 0,
                art_url: String::new(),
                    info: serde_json::json!({ "genres": g.genres }),
            };
            store::library_add(&game);
            store::library_set_owned(&g.id, true);
            seen.insert(g.id.clone());
        }
        for old in store::library_all() {
            if old.owned && !seen.contains(&old.id) {
                store::library_set_owned(&old.id, false);
            }
        }
        // Enriquecer precio/tamaño desde el catálogo público.
        for g in games {
            let row = store::library_all()
                .into_iter()
                .find(|r| r.id == g.id);
            let needs = row.as_ref().map(|r| r.price.is_empty() || r.package.is_empty()).unwrap_or(false);
            if !needs {
                continue;
            }
            if let Ok(list) = oculusdb::search_store(&g.name, 10) {
                if let Some(pick) = list.into_iter().find(|r| r.id == g.id) {
                    let mut full = row.unwrap();
                    full.price = pick.price;
                    full.size = pick.size;
                    full.package = pick.package;
                    full.publisher = pick.publisher;
                    full.store_url = pick.store_url;
                    full.info = serde_json::json!({
                        "genres": pick.genres, "image_url": pick.image_url,
                    });
                    store::library_add(&full);
                }
            }
        }
        self.syncing = false;
        self.refresh_library();
        self.ensure_covers();
    }

    fn do_unlink(&mut self) {
        meta_auth::forget();
        store::set_setting("meta_name", "");
        self.account.clear();
    }

    fn do_link(&mut self) {
        if meta_auth::is_linked() {
            self.do_unlink();
            return;
        }
        self.linking = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            tx.send(AsyncMsg::Linked(meta_auth::sign_in())).ok();
        });
    }

    fn open_download_dialog(&mut self, app: store::Game) {
        if self.plan_pending {
            return;
        }
        self.plan_pending = true;
        let tx = self.tx.clone();
        let id = app.id.clone();
        std::thread::spawn(move || {
            let r = (|| -> Result<meta_auth::Plan, String> {
                let token = meta_auth::profile_token()?;
                meta_auth::plan(&token, &id)
            })();
            tx.send(AsyncMsg::Plan(id, r)).ok();
        });
        // guarda la app para cuando llegue el plan (sin abrir la ficha detalle)
        self.plan_app = Some(app);
    }

    fn start_download(&mut self, app: &store::Game, extra: Vec<meta_auth::PlanFile>) {
        let folder = game_folder(&app.name);
        std::fs::create_dir_all(&folder).ok();
        oculusdb::save_json(&folder.join(format!("{}.json", app.id)), &to_store_app(app));
        let tx = self.tx.clone();
        let dm = self.dm.clone();
        let app_id = app.id.clone();
        std::thread::spawn(move || {
            let r = (|| -> Result<(), String> {
                let token = meta_auth::profile_token()?;
                let plan = meta_auth::plan(&token, &app_id)?;
                let delivery = meta_auth::delivery_token(&token)?;
                let mut files = plan.files;
                files.extend(extra);
                for f in &files {
                    let name = std::path::Path::new(&f.name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() || name == "." || name == ".." {
                        continue;
                    }
                    let url = meta_auth::file_url(f, &delivery)?;
                    let dest = folder.join(&name);
                    let jid = dm.add(&dm, format!("{} · {name}", plan.name), url, dest);
                    tx.send(AsyncMsg::Linked(Ok(format!("JOB:{app_id}:{jid}")))).ok();
                }
                Ok(())
            })();
            if let Err(e) = r {
                tx.send(AsyncMsg::Linked(Err(format!("DL:{e}")))).ok();
            }
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let lang = self.lang.clone();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AsyncMsg::Search(r) => {
                    self.results = r;
                    self.searching = false;
                }
                AsyncMsg::Sync(alias, uid, games) => self.apply_sync(alias, uid, games),
                AsyncMsg::Plan(id, r) => {
                    self.plan_pending = false;
                    match r {
                        Ok(p) => {
                            // Base única: requeridos + opcionales no-idioma.
                            // Cada idioma: un paquete global (textos + audio).
                            // Se omiten los archivos de idioma ya incluidos en la base.
                            let mut base = p.files;
                            for f in p.optional.iter().filter(|f| !is_lang_file(&f.name)) {
                                base.push(f.clone());
                            }
                            // Solo se omiten opcionales idénticos a la base
                            // (mismo nombre o mismo idioma+tipo+tamaño).
                            let mut base_names: std::collections::HashSet<String> =
                                std::collections::HashSet::new();
                            let mut base_sig: std::collections::HashSet<(
                                String,
                                String,
                                u64,
                            )> = std::collections::HashSet::new();
                            for f in &base {
                                base_names.insert(f.name.clone());
                                if let Some(l) = lang_detect(&f.name) {
                                    base_sig.insert((
                                        l,
                                        lang_kind(&f.name).to_string(),
                                        f.size,
                                    ));
                                }
                            }
                            let mut order: Vec<String> = vec![];
                            let mut map: HashMap<String, Vec<meta_auth::PlanFile>> =
                                HashMap::new();
                            for f in p.optional.into_iter().filter(|f| is_lang_file(&f.name)) {
                                let Some(key) = lang_detect(&f.name) else {
                                    // Localización compartida: parte de la base.
                                    base.push(f);
                                    continue;
                                };
                                if base_names.contains(&f.name) {
                                    continue;
                                }
                                if let Some(l) = lang_detect(&f.name) {
                                    let sig = (
                                        l,
                                        lang_kind(&f.name).to_string(),
                                        f.size,
                                    );
                                    if base_sig.contains(&sig) {
                                        continue;
                                    }
                                }
                                if !map.contains_key(&key) {
                                    order.push(key.clone());
                                }
                                map.entry(key).or_default().push(f);
                            }
                            let mut langs: Vec<(bool, String, Vec<meta_auth::PlanFile>)> = order
                                .into_iter()
                                .filter_map(|k| map.remove(&k).map(|v| (false, k, v)))
                                .collect();
                            langs.sort_by(|a, b| {
                                a.1.to_lowercase().cmp(&b.1.to_lowercase())
                            });
                            let langs = langs;
                            if let Some(app) = self.plan_app.clone().filter(|a| a.id == id) {
                                let _ = app;
                                self.plan_dlg = Some(PlanDlg {
                                    app: self
                                        .library
                                        .iter()
                                        .find(|g| g.id == id)
                                        .cloned()
                                        .unwrap_or_else(|| self.plan_app.clone().unwrap()),
                                    base,
                                    langs,
                                });
                            }
                            self.plan_app = None;
                        }
                        Err(_) => {
                            self.plan_dlg = None;
                        }
                    }
                    let _ = id;
                }
                AsyncMsg::Cover(id, bytes) => self.load_cover(ctx, &id, &bytes),
                AsyncMsg::Linked(r) => match r {
                    Ok(s) if s.starts_with("JOB:") => {
                        let parts: Vec<&str> = s.split(':').collect();
                        if parts.len() == 3 {
                            self.game_jobs
                                .entry(parts[1].to_string())
                                .or_default()
                                .push(parts[2].to_string());
                        }
                    }
                    Ok(alias) => {
                        self.linking = false;
                        if !alias.is_empty() {
                            store::set_setting("meta_name", &alias);
                            self.account = alias;
                        }
                        self.do_sync();
                    }
                    Err(e) => {
                        self.linking = false;
                        if let Some(msg) = e.strip_prefix("DL:") {
                            // error de descarga: modal simple
                            self.plan_dlg = None;
                            egui::Window::new(tr(&lang, "Sin descarga"))
                                .collapsible(false)
                                .resizable(false)
                                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                                .show(ctx, |ui| {
                                    ui.label(msg);
                                });
                        }
                    }
                },
            }
        }
        // cargar portadas ya en disco
        for g in store::library_all() {
            if !self.covers.contains_key(&g.id) {
                let p = store::covers_dir().join(format!("{}.jpg", g.id));
                if let Ok(bytes) = std::fs::read(&p) {
                    self.load_cover(ctx, &g.id, &bytes);
                }
            }
        }
        // repintar mientras haya descargas activas
        let active = self
            .dm
            .snapshot()
            .iter()
            .any(|j| matches!(j.status, JobStatus::Queued | JobStatus::Downloading | JobStatus::Paused));
        if active || self.searching || self.syncing || self.linking {
            ctx.request_repaint();
        }
    }
}

fn void_msg() {}

fn game_folder(name: &str) -> std::path::PathBuf {
    let safe: String = name
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let safe = safe.trim();
    let safe = if safe.is_empty() { "juego" } else { safe };
    store::download_dir().join(safe)
}

fn to_store_app(g: &store::Game) -> oculusdb::StoreApp {
    oculusdb::StoreApp {
        id: g.id.clone(),
        name: g.name.clone(),
        package: g.package.clone(),
        price: g.price.clone(),
        size: g.size.clone(),
        publisher: g.publisher.clone(),
        genres: g.info.get("genres").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        store_url: g.store_url.clone(),
        image_url: g.info.get("image_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    }
}

fn setup_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [("SegoeUI", "segoeui.ttf"), ("SegoeUI-Bold", "segoeuib.ttf")] {
        let p = std::path::Path::new(&windir).join("Fonts").join(file);
        if let Ok(bytes) = std::fs::read(p) {
            fonts.font_data.insert(name.into(), egui::FontData::from_owned(bytes).into());
        }
    }
    if fonts.font_data.contains_key("SegoeUI") {
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(fam)
                .or_default()
                .insert(0, "SegoeUI".into());
        }
        ctx.set_fonts(fonts);
    }
}

fn open_url(url: &str) {
    std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
        .ok();
}

fn open_folder(path: &std::path::Path) {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .ok();
}

fn card_hit_rects(card: egui::Rect) -> (egui::Rect, egui::Rect) {
    let isz = 30.0;
    let iy = card.max.y - 52.0 + 26.0 - isz / 2.0;
    let web = egui::Rect::from_min_size(
        egui::pos2(card.max.x - 8.0 - isz, iy),
        egui::vec2(isz, isz),
    );
    let dl = egui::Rect::from_min_size(
        egui::pos2(card.max.x - 8.0 - isz - 10.0 - isz, iy),
        egui::vec2(isz, isz),
    );
    (web, dl)
}

fn sys_lang() -> String {
    let mut buf = [0u16; 85];
    let n = unsafe {
        windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf)
    };
    if n > 1 {
        let s = String::from_utf16_lossy(&buf[..(n as usize - 1).min(buf.len())]);
        if s.to_lowercase().starts_with("es") {
            return "es".into();
        }
    }
    "en".into()
}

fn eff_lang() -> String {
    match store::get_setting("lang").as_str() {
        "es" | "en" => store::get_setting("lang"),
        _ => sys_lang(),
    }
}

static CUR_LANG: std::sync::OnceLock<std::sync::Mutex<String>> =
    std::sync::OnceLock::new();

pub fn cur_lang() -> String {
    CUR_LANG
        .get()
        .and_then(|m| m.lock().ok())
        .map(|g| g.clone())
        .unwrap_or_else(sys_lang)
}

pub fn set_cur_lang(lang: &str) {
    match CUR_LANG.get() {
        Some(m) => {
            if let Ok(mut g) = m.lock() {
                *g = lang.to_string();
            }
        }
        None => {
            let _ = CUR_LANG.set(std::sync::Mutex::new(lang.to_string()));
        }
    }
}

/// Devuelve el texto en el idioma activo ("en" traduce, resto conserva).
fn tr(lang: &str, es: &str) -> String {
    if lang != "en" {
        return es.to_string();
    }
    match es {
        "Archivo" => "File",
        "Actualizar biblioteca  F5" => "Refresh library  F5",
        "Salir  Alt+F4" => "Exit  Alt+F4",
        "Ver" => "View",
        "Biblioteca  Ctrl+1" => "Library  Ctrl+1",
        "Tienda  Ctrl+2" => "Store  Ctrl+2",
        "Descargas  Ctrl+3" => "Downloads  Ctrl+3",
        "Idioma" => "Language",
        "(actual)" => "(current)",
        "Sin vincular" => "Not linked",
        "Vinculada" => "Linked",
        "Vincular cuenta Meta" => "Link Meta account",
        "Desvincular cuenta Meta" => "Unlink Meta account",
        "Buscar en la tienda…  (Ctrl+F)" => "Search the store…  (Ctrl+F)",
        "Actualizar" => "Refresh",
        "Sincronizar (F5)" => "Sync (F5)",
        "Descargar" => "Download",
        "Descargar el juego seleccionado" => "Download the selected game",
        "Carpeta" => "Folder",
        "Abrir carpeta de descargas" => "Open downloads folder",
        "Abrir carpeta" => "Open folder",
        "Eliminar" => "Delete",
        "Eliminar de la Biblioteca (Supr)" => "Delete from library (Del)",
        "Biblioteca" => "Library",
        "Tienda" => "Store",
        "Descargas" => "Downloads",
        "Tus juegos (Ctrl+1)" => "Your games (Ctrl+1)",
        "Buscar en la Store (Ctrl+2)" => "Search the Store (Ctrl+2)",
        "Descargas en curso (Ctrl+3)" => "Downloads in progress (Ctrl+3)",
        "CUENTA" => "ACCOUNT",
        "de" => "of",
        "vinculados" => "linked",
        "Detalles" => "Details",
        "Selecciona un juego" => "Select a game",
        "Estado" => "Status",
        "Precio" => "Price",
        "Tamaño" => "Size",
        "Editor" => "Publisher",
        "Paquete" => "Package",
        "Vinculado" => "Linked",
        "Vinculado a tu cuenta" => "Linked to your account",
        "No vinculado" => "Not linked",
        "Sin comprobar" => "Unchecked",
        "Descargado" => "Downloaded",
        "Abrir en la Store" => "Open in Store",
        "Abrir carpeta de descarga" => "Open download folder",
        "Listo" => "Ready",
        "juegos" => "games",
        "col" => "cols",
        "en curso…" => "in progress…",
        "F5 actualizar · Supr eliminar · F1 ayuda" => "F5 refresh · Del delete · F1 help",
        "Buscar en la biblioteca…" => "Search library…",
        "Actualizar Biblioteca" => "Refresh library",
        "Importar lista…" => "Import list…",
        "Busca juegos de la Meta Quest Store…" => "Search Meta Quest Store games…",
        "Buscar" => "Search",
        "Añadir a Biblioteca" => "Add to library",
        "Pausar" => "Pause",
        "Reanudar" => "Resume",
        "Cancelar" => "Cancel",
        "Sin descargas. Descarga desde una ficha." => "No downloads yet. Download from a game card.",
        "¿Quitar «{n}» de la Biblioteca?" => "Remove «{n}» from the library?",
        "Importar mis juegos" => "Import my games",
        "Pega tus juegos (uno por línea):" => "Paste your games (one per line):",
        "Importar" => "Import",
        "Contenido base" => "Base content",
        "archivos de datos" => "data files",
        "siempre incluido" => "always included",
        "Idiomas (opcional)" => "Languages (optional)",
        "Sin datos opcionales." => "No optional data.",
        "Total" => "Total",
        "más" => "more",
        "Sin descarga" => "No download",
        "No se pudo abrir el navegador" => "Could not open the browser",
        "Sin nombre" => "Unnamed",
        "Gratis" => "Free",
        "en cola" => "queued",
        "descargando" => "downloading",
        "pausada" => "paused",
        "hecha" => "done",
        "cancelada" => "cancelled",
        "textos" => "text",
        "datos" => "data",
        "arch." => "files",
        "Inglés" => "English",
        "Francés" => "French",
        "Alemán" => "German",
        "Italiano" => "Italian",
        "Español" => "Spanish",
        "Español (Latam)" => "Spanish (Latam)",
        "Japonés" => "Japanese",
        "Coreano" => "Korean",
        "Portugués" => "Portuguese",
        "Ruso" => "Russian",
        "Chino" => "Chinese",
        "Neerlandés" => "Dutch",
        "Polaco" => "Polish",
        "Árabe" => "Arabic",
        "Turco" => "Turkish",
        "Sueco" => "Swedish",
        "Danés" => "Danish",
        "Noruego" => "Norwegian",
        "Finés" => "Finnish",
        "Checo" => "Czech",
        "Húngaro" => "Hungarian",
        "Griego" => "Greek",
        "Tailandés" => "Thai",
        "Vietnamita" => "Vietnamese",
        "Indonesio" => "Indonesian",
        "Hebreo" => "Hebrew",
        "Hindi" => "Hindi",
        _ => es,
    }
    .to_string()
}

fn cover_uv(w: f32, h: f32) -> egui::Rect {
    let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    if w <= 0.0 || h <= 0.0 {
        return full;
    }
    let a = w / h;
    if (a - 1.0).abs() < 0.02 {
        return full;
    }
    if a > 1.0 {
        let cw = 1.0 / a;
        let x0 = (1.0 - cw) / 2.0;
        egui::Rect::from_min_max(egui::pos2(x0, 0.0), egui::pos2(x0 + cw, 1.0))
    } else {
        let y0 = (1.0 - a) / 2.0;
        egui::Rect::from_min_max(egui::pos2(0.0, y0), egui::pos2(1.0, y0 + a))
    }
}

fn load_ui_icon(
    ctx: &egui::Context,
    bytes: &[u8],
    crop_bottom: f32,
    name: &str,
) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    let keep_h = ((h as f32 * (1.0 - crop_bottom)) as u32).max(1);
    // Estilo viejo (fondo blanco opaco): el blanco es fondo -> transparente.
    // Estilo nuevo (fondo ya transparente): se conserva tal cual.
    let corners = [
        img.get_pixel(0, 0),
        img.get_pixel(w - 1, 0),
        img.get_pixel(0, keep_h - 1),
        img.get_pixel(w - 1, keep_h - 1),
    ];
    let old_style = corners
        .iter()
        .filter(|p| {
            let a = p.0;
            a[3] > 128 && a[0] > 235 && a[1] > 235 && a[2] > 235
        })
        .count()
        >= 3;
    let mut buf = Vec::with_capacity((w * keep_h * 4) as usize);
    for y in 0..keep_h {
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            if !old_style {
                buf.extend_from_slice(&p);
            } else if p[0] > 235 && p[1] > 235 && p[2] > 235 {
                buf.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                // Icono en blanco puro, conserva el alfa original (antialias).
                buf.extend_from_slice(&[255, 255, 255, p[3]]);
            }
        }
    }
    let color =
        egui::ColorImage::from_rgba_unmultiplied([w as usize, keep_h as usize], &buf);
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

fn lang_detect(name: &str) -> Option<String> {
    let n = name.to_lowercase();
    if n.contains("english") || n.contains("en-us") {
        Some("Inglés".into())
    } else if n.contains("french") {
        Some("Francés".into())
    } else if n.contains("german") {
        Some("Alemán".into())
    } else if n.contains("italian") {
        Some("Italiano".into())
    } else if n.contains("spanish") || n.contains("es-es") || n.contains("spain") {
        if n.contains("latam") || n.contains("419") {
            Some("Español (Latam)".into())
        } else {
            Some("Español".into())
        }
    } else if n.contains("japanese") || n.contains("ja-jp") {
        Some("Japonés".into())
    } else if n.contains("korean") || n.contains("ko-kr") {
        Some("Coreano".into())
    } else if n.contains("portuguese") {
        Some("Portugués".into())
    } else if n.contains("russian") {
        Some("Ruso".into())
    } else if n.contains("chinese") {
        Some("Chino".into())
    } else if n.contains("dutch") {
        Some("Neerlandés".into())
    } else if n.contains("polish") {
        Some("Polaco".into())
    } else if n.contains("arabic") {
        Some("Árabe".into())
    } else if n.contains("turkish") {
        Some("Turco".into())
    } else if n.contains("swedish") {
        Some("Sueco".into())
    } else if n.contains("danish") {
        Some("Danés".into())
    } else if n.contains("norwegian") {
        Some("Noruego".into())
    } else if n.contains("finnish") {
        Some("Finés".into())
    } else if n.contains("czech") {
        Some("Checo".into())
    } else if n.contains("hungarian") {
        Some("Húngaro".into())
    } else if n.contains("greek") {
        Some("Griego".into())
    } else if n.contains("thai") {
        Some("Tailandés".into())
    } else if n.contains("vietnamese") {
        Some("Vietnamita".into())
    } else if n.contains("indonesian") {
        Some("Indonesio".into())
    } else if n.contains("hebrew") {
        Some("Hebreo".into())
    } else if n.contains("hindi") {
        Some("Hindi".into())
    } else {
        None
    }
}

fn is_lang_file(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("localiz")
        || n.contains("voice")
        || n.contains("subtitle")
        || n.contains("strings")
        || (n.starts_with("audio") && (n.contains('-') || n.contains('_')))
}

fn lang_kind(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("string") || n.contains("text") {
        "textos"
    } else if n.contains("audio") || n.ends_with(".obb") {
        "audio"
    } else {
        "datos"
    }
}

fn mb(bytes: u64) -> String {
    if bytes == 0 {
        "?".into()
    } else if bytes >= 1073741824 {
        format!("{:.2} GB", bytes as f64 / 1073741824.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1048576.0)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        let lang = self.lang.clone();
        let (down, hpos) =
            ctx.input(|i| (i.pointer.primary_down(), i.pointer.hover_pos()));
        if self.page == Page::Lib {
            if down && !self.press_down {
                self.press_id = hpos.and_then(|p| {
                    self.hits
                        .iter()
                        .rfind(|(_, c, _, _)| c.contains(p))
                        .map(|(id, _, _, _)| id.clone())
                });
            }
            if !down && self.press_down {
                if let (Some(pid), Some(p)) = (self.press_id.clone(), hpos) {
                    if let Some((id, c, w, d)) = self
                        .hits
                        .iter()
                        .find(|(id, _, _, _)| id == &pid)
                        .cloned()
                    {
                        if w.contains(p) {
                            if let Some(g) =
                                self.library.iter().find(|x| x.id == id).cloned()
                            {
                                open_url(&g.store_url);
                            }
                        } else if d.contains(p) {
                            if let Some(g) =
                                self.library.iter().find(|x| x.id == id).cloned()
                            {
                                if game_folder(&g.name).join("base.apk").exists() {
                                    open_folder(&game_folder(&g.name));
                                } else {
                                    self.open_download_dialog(g);
                                }
                            }
                        } else if c.contains(p) {
                            self.selected = Some(id.clone());
                        }
                    }
                }
                self.press_id = None;
            }
            self.press_down = down;
            if let Some(p) = hpos {
                if self
                    .hits
                    .iter()
                    .any(|(_, _, w, d)| w.contains(p) || d.contains(p))
                {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        } else {
            self.press_down = down;
            self.press_id = None;
        }
        let panel = egui::Color32::from_rgb(0x1a, 0x1a, 0x1a);
        let card = egui::Color32::from_rgb(0x2d, 0x2d, 0x2d);
        let border = egui::Color32::from_rgb(0x3a, 0x3a, 0x3a);
        let txt = egui::Color32::from_rgb(0xec, 0xec, 0xec);
        let accent = egui::Color32::from_rgb(0x00, 0x82, 0xFB);
        let _ = panel;

        // Atajos globales
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.do_sync();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            // reservado: ayuda
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num1)) {
            self.page = Page::Lib;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num2)) {
            self.page = Page::Store;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num3)) {
            self.page = Page::Dl;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::L)) {
            self.do_link();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Delete) && !ctx.wants_keyboard_input()) {
            if let Some(id) = self.selected.clone() {
                store::library_remove(&id);
                self.selected = None;
                self.refresh_library();
            }
        }

        // Menú
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(tr(&lang, "Archivo"), |ui| {
                    if ui.button(tr(&lang, "Actualizar biblioteca  F5")).clicked() {
                        self.do_sync();
                        ui.close_menu();
                    }
                    if ui.button(tr(&lang, "Salir  Alt+F4")).clicked() {
                        std::process::exit(0);
                    }
                });
                ui.menu_button(tr(&lang, "Ver"), |ui| {
                    if ui.button(tr(&lang, "Biblioteca  Ctrl+1")).clicked() {
                        self.page = Page::Lib;
                        ui.close_menu();
                    }
                    if ui.button(tr(&lang, "Tienda  Ctrl+2")).clicked() {
                        self.page = Page::Store;
                        ui.close_menu();
                    }
                    if ui.button(tr(&lang, "Descargas  Ctrl+3")).clicked() {
                        self.page = Page::Dl;
                        ui.close_menu();
                    }
                });
                ui.menu_button(tr(&lang, "Idioma"), |ui| {
                    for code in ["es", "en"] {
                        let name = if code == "es" { "Español" } else { "English" };
                        let label = if self.lang == code {
                            format!("{name} {}", tr(&lang, "(actual)"))
                        } else {
                            name.to_string()
                        };
                        if ui.button(label).clicked() {
                            store::set_setting("lang", code);
                            self.lang = code.to_string();
                            set_cur_lang(code);
                            ui.close_menu();
                        }
                    }
                });
            });
        });

        // Cabecera + command bar
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("QD");
                let label = if meta_auth::is_linked() {
                    if self.account.is_empty() {
                        format!("● {}", tr(&lang, "Vinculado"))
                    } else {
                        format!("● {} ({})", tr(&lang, "Vinculada"), self.account)
                    }
                } else {
                    format!("○ {}", tr(&lang, "Sin vincular"))
                };
                ui.label(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if meta_auth::is_linked() {
                        if ui.button(tr(&lang, "Desvincular cuenta Meta")).clicked() {
                            self.do_unlink();
                        }
                    } else if ui.button(tr(&lang, "Vincular cuenta Meta")).clicked() {
                        self.do_link();
                    }
                    let mut q = std::mem::take(&mut self.query);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut q)
                            .hint_text(tr(&lang, "Buscar en la tienda…  (Ctrl+F)"))
                            .desired_width(220.0),
                    );
                    self.query = q;
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.page = Page::Store;
                        self.do_search();
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui.button(format!("⟳ {}", tr(&lang, "Actualizar"))).on_hover_text(tr(&lang, "Sincronizar (F5)")).clicked() {
                    self.do_sync();
                }
                let sel = self.selected_game();
                if ui
                    .add_enabled(sel.is_some(), egui::Button::new(format!("⬇ {}", tr(&lang, "Descargar"))))
                    .on_hover_text(tr(&lang, "Descargar el juego seleccionado"))
                    .clicked()
                {
                    if let Some(g) = sel {
                        self.open_download_dialog(g);
                    }
                }
                if ui.button(format!("📂 {}", tr(&lang, "Carpeta"))).on_hover_text(tr(&lang, "Abrir carpeta de descargas")).clicked() {
                    std::fs::create_dir_all(store::download_dir()).ok();
                    open_folder(&store::download_dir());
                }
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new(format!("🗑 {}", tr(&lang, "Eliminar"))))
                    .on_hover_text(tr(&lang, "Eliminar de la Biblioteca (Supr)"))
                    .clicked()
                {
                    self.confirm_del = true;
                }
            });
        });

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::F)) {
            // foco a búsqueda global: la cabecera ya la muestra
        }

        // Nav lateral
        egui::SidePanel::left("nav")
            .exact_width(196.0)
            .show(ctx, |ui| {
                for (p, label, tip) in [
                    (Page::Lib, format!("📚 {}", tr(&lang, "Biblioteca")), tr(&lang, "Tus juegos (Ctrl+1)")),
                    (Page::Store, format!("🏬 {}", tr(&lang, "Tienda")), tr(&lang, "Buscar en la Store (Ctrl+2)")),
                    (Page::Dl, format!("⬇ {}", tr(&lang, "Descargas")), tr(&lang, "Descargas en curso (Ctrl+3)")),
                ] {
                    let mut text = label.to_string();
                    if p == Page::Dl {
                        let n = self
                            .dm
                            .snapshot()
                            .iter()
                            .filter(|j| {
                                matches!(
                                    j.status,
                                    JobStatus::Queued
                                        | JobStatus::Downloading
                                        | JobStatus::Paused
                                )
                            })
                            .count();
                        if n > 0 {
                            text = format!("⬇ {} ({n})", tr(&lang, "Descargas"));
                        }
                    }
                    let mut btn = egui::Button::new(text).min_size(egui::vec2(180.0, 34.0));
                    if self.page == p {
                        btn = btn.fill(card);
                    }
                    if ui.add(btn).on_hover_text(tip).clicked() {
                        self.page = p;
                    }
                }
                ui.add_space(12.0);
                ui.label(egui::RichText::new(tr(&lang, "CUENTA")).small().color(egui::Color32::GRAY));
                ui.label(if self.account.is_empty() {
                    if meta_auth::is_linked() {
                        format!("● {}", tr(&lang, "Vinculado"))
                    } else {
                        format!("○ {}", tr(&lang, "Sin vincular"))
                    }
                } else {
                    format!("● {}", self.account)
                });
                let libs = store::library_all();
                let n = libs.iter().filter(|g| g.owned).count();
                ui.label(
                    egui::RichText::new(format!(
                        "{n} {} {} {}",
                        tr(&lang, "de"),
                        libs.len(),
                        tr(&lang, "vinculados")
                    ))
                    .small(),
                );
            });

        // Inspector
        egui::SidePanel::right("insp")
            .exact_width(248.0)
            .show(ctx, |ui| {
                ui.heading(tr(&lang, "Detalles"));
                if let Some(g) = self.selected_game() {
                    ui.label(egui::RichText::new(&g.name).strong());
                    egui::Grid::new("insp-grid").num_columns(2).show(ui, |ui| {
                        for (k, v) in [
                            (tr(&lang, "Estado"), if g.owned { tr(&lang, "Vinculado a tu cuenta") } else { tr(&lang, "No vinculado") }),
                            (tr(&lang, "Precio"), if g.price.is_empty() { "?".to_string() } else { g.price.clone() }),
                            (tr(&lang, "Tamaño"), if g.size.is_empty() { "?".to_string() } else { g.size.clone() }),
                            (tr(&lang, "Editor"), if g.publisher.is_empty() { "?".to_string() } else { g.publisher.clone() }),
                            (tr(&lang, "Paquete"), if g.package.is_empty() { "?".to_string() } else { g.package.clone() }),
                        ] {
                            ui.label(k);
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                    if ui.button(format!("⬇ {}", tr(&lang, "Descargar"))).clicked() {
                        self.open_download_dialog(g);
                    }
                    if ui.button(tr(&lang, "Abrir en la Store")).clicked() {
                        open_url(&self.selected_game().map(|x| x.store_url).unwrap_or_default());
                    }
                } else {
                    ui.label(tr(&lang, "Selecciona un juego"));
                }
            });

        // Status bar
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let snap = self.dm.snapshot();
                let act: Vec<_> = snap
                    .iter()
                    .filter(|j| {
                        matches!(
                            j.status,
                            JobStatus::Queued | JobStatus::Downloading | JobStatus::Paused
                        )
                    })
                    .collect();
                if act.is_empty() {
                    let libs = store::library_all();
                    let n = libs.iter().filter(|g| g.owned).count();
                    ui.label(format!(
                        "{} · {} {} ({} {}) · {} {}",
                        tr(&lang, "Listo"),
                        libs.len(),
                        tr(&lang, "juegos"),
                        n,
                        tr(&lang, "vinculados"),
                        self.ncols,
                        tr(&lang, "col")
                    ));
                } else {
                    let done: u64 = act.iter().map(|j| j.done).sum();
                    let tot: u64 = act.iter().map(|j| j.total).sum();
                    if tot > 0 {
                        ui.label(format!("⬇ {} · {:.0} %", act[0].name, done as f64 / tot as f64 * 100.0));
                    } else {
                        ui.label(format!("⬇ {} {}", act.len(), tr(&lang, "en curso…")));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.available_width() > 280.0 {
                        ui.label(tr(&lang, "F5 actualizar · Supr eliminar · F1 ayuda"));
                    }
                });
            });
        });

        // Contenido
        egui::CentralPanel::default().show(ctx, |ui| match self.page {
            Page::Lib => self.ui_library(ui, txt, card, border, accent),
            Page::Store => self.ui_store(ui),
            Page::Dl => self.ui_downloads(ui),
        });

        // Modales
        if self.confirm_del {
            let id = self.selected.clone().unwrap_or_default();
            let name = self
                .library
                .iter()
                .find(|g| g.id == id)
                .map(|g| g.name.clone())
                .unwrap_or_default();
            egui::Window::new(tr(&lang, "Eliminar"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(tr(&lang, "¿Quitar «{n}» de la Biblioteca?").replace("{n}", &name));
                    ui.horizontal(|ui| {
                        if ui.button(tr(&lang, "Eliminar")).clicked() {
                            store::library_remove(&id);
                            self.selected = None;
                            self.refresh_library();
                            self.confirm_del = false;
                        }
                        if ui.button(tr(&lang, "Cancelar")).clicked() {
                            self.confirm_del = false;
                        }
                    });
                });
        }
        if self.import_open {
            egui::Window::new(tr(&lang, "Importar mis juegos"))
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(tr(&lang, "Pega tus juegos (uno por línea):"));
                    ui.text_edit_multiline(&mut self.import_text);
                    ui.horizontal(|ui| {
                        if ui.button(tr(&lang, "Importar")).clicked() {
                            let names: Vec<String> = self
                                .import_text
                                .lines()
                                .map(|l| l.trim().to_string())
                                .filter(|l| !l.is_empty())
                                .collect();
                            self.import_open = false;
                            let tx = self.tx.clone();
                            std::thread::spawn(move || {
                                for name in names {
                                    if let Ok(list) = oculusdb::search_store(&name, 5) {
                                        let pick = list
                                            .iter()
                                            .find(|r| r.name.to_lowercase() == name.to_lowercase())
                                            .or_else(|| list.first());
                                        if let Some(p) = pick {
                                            let g = store::Game {
                                                id: p.id.clone(),
                                                name: p.name.clone(),
                                                package: p.package.clone(),
                                                price: p.price.clone(),
                                                size: p.size.clone(),
                                                publisher: p.publisher.clone(),
                                                store_url: p.store_url.clone(),
                                                owned: false,
                                                verified_at: 0,
                art_url: String::new(),
                                                                                    info: serde_json::json!({
                                                    "genres": p.genres, "image_url": p.image_url,
                                                }),
                                            };
                                            store::library_add(&g);
                                        }
                                    }
                                }
                                tx.send(AsyncMsg::Search(vec![])).ok();
                            });
                        }
                        if ui.button(tr(&lang, "Cancelar")).clicked() {
                            self.import_open = false;
                        }
                    });
                });
        }
        if self.plan_dlg.is_some() {
            let mut go: Option<Vec<meta_auth::PlanFile>> = None;
            let mut close = false;
            if let Some(d) = self.plan_dlg.as_mut() {
                let title = format!("{} {}", tr(&lang, "Descargar"), d.app.name);
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .min_width(620.0)
                    .max_width(620.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        let base_total: u64 = d.base.iter().map(|f| f.size).sum();
                        let apk = d
                            .base
                            .iter()
                            .find(|f| f.name.ends_with(".apk"))
                            .map(|f| f.name.clone())
                            .unwrap_or_else(|| "base.apk".into());
                        let n_data = d.base.len().saturating_sub(1);
                        ui.label(egui::RichText::new(tr(&lang, "Contenido base")).strong());
                        ui.label(format!(
                            "{} + {} {} — {} ({})",
                            apk,
                            n_data,
                            tr(&lang, "archivos de datos"),
                            mb(base_total),
                            tr(&lang, "siempre incluido")
                        ));
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical()
                            .max_height(380.0)
                            .show(ui, |ui| {
                                if d.langs.is_empty() {
                                    ui.label(tr(&lang, "Sin datos opcionales."));
                                } else {
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(tr(&lang, "Idiomas (opcional)"))
                                            .strong(),
                                    );
                                    egui::Grid::new("langrows")
                                        .num_columns(3)
                                        .striped(true)
                                        .spacing([8.0, 2.0])
                                        .show(ui, |ui| {
                                            for (sel, pack, files) in
                                                d.langs.iter_mut()
                                            {
                                                let total: u64 = files
                                                    .iter()
                                                    .map(|f| f.size)
                                                    .sum();
                                                let mut kinds: Vec<String> = files
                                                    .iter()
                                                    .map(|f| tr(&lang, lang_kind(&f.name)))
                                                    .collect();
                                                kinds.sort();
                                                kinds.dedup();
                                                let title = format!(
                                                    "{} — {} ({} {})",
                                                    tr(&lang, pack),
                                                    kinds.join(" + "),
                                                    files.len(),
                                                    tr(&lang, "arch.")
                                                );
                                                let tip = if files.len() <= 12 {
                                                    files
                                                        .iter()
                                                        .map(|f| f.name.clone())
                                                        .collect::<Vec<_>>()
                                                        .join("\n")
                                                } else {
                                                    format!(
                                                        "{}…\n(+{} {})",
                                                        files
                                                            .iter()
                                                            .take(12)
                                                            .map(|f| f.name.clone())
                                                            .collect::<Vec<_>>()
                                                            .join("\n"),
                                                        files.len() - 12,
                                                        tr(&lang, "más")
                                                    )
                                                };
                                                ui.checkbox(sel, "");
                                                ui.add_sized(
                                                    egui::vec2(360.0, 0.0),
                                                    egui::Label::new(
                                                        egui::RichText::new(title),
                                                    )
                                                    .truncate(),
                                                )
                                                .on_hover_text(tip);
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        ui.label(mb(total));
                                                    },
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }
                            });
                        let sel_total: u64 = d
                            .langs
                            .iter()
                            .filter(|(s, _, _)| *s)
                            .flat_map(|(_, _, fs)| fs)
                            .map(|f| f.size)
                            .sum();
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{}: {}",
                                tr(&lang, "Total"),
                                mb(base_total + sel_total)
                            ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(tr(&lang, "Cancelar")).clicked() {
                                        close = true;
                                    }
                                    if ui.button(tr(&lang, "Descargar")).clicked() {
                                        go = Some(
                                            d.langs
                                                .iter()
                                                .filter(|(s, _, _)| *s)
                                                .flat_map(|(_, _, fs)| fs.clone())
                                                .collect(),
                                        );
                                        close = true;
                                    }
                                },
                            );
                        });
                    });
            }
            if let Some(sel) = go {
                let app = self.plan_dlg.as_ref().map(|d| d.app.clone());
                self.plan_dlg = None;
                if let Some(app) = app {
                    self.start_download(&app, sel);
                }
            } else if close {
                self.plan_dlg = None;
            }
        }
    }
}

impl App {
    fn ui_library(&mut self, ui: &mut egui::Ui, txt: egui::Color32, card: egui::Color32, border: egui::Color32, accent: egui::Color32) {
        let lang = self.lang.clone();
        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr(&lang, "Buscar en la biblioteca…"))
                    .desired_width(220.0),
            );
            if ui.button(tr(&lang, "Actualizar Biblioteca")).clicked() {
                self.do_sync();
            }
            if ui.button(tr(&lang, "Importar lista…")).clicked() {
                self.import_open = true;
            }
            if ui.button(tr(&lang, "Eliminar")).clicked() {
                self.confirm_del = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let libs = store::library_all();
                let n = libs.iter().filter(|g| g.owned).count();
                ui.label(format!(
                    "{n} {} {} {}",
                    tr(&lang, "de"),
                    libs.len(),
                    tr(&lang, "vinculados")
                ));
            });
        });
        let q = self.filter.to_lowercase();
        let mut libs: Vec<store::Game> = store::library_all()
            .into_iter()
            .filter(|g| g.name.to_lowercase().contains(&q))
            .collect();
        libs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.library = store::library_all();
        self.hits.clear();
        // Columnas dinámicas (2-5): a tamaño completo entran 5
        let ncols = ((ui.available_width() / 160.0).round() as usize).clamp(2, 5);
        self.ncols = ncols;
        let cw = ((ui.available_width() - 48.0) / ncols as f32)
            .floor()
            .clamp(60.0, 280.0);
        let tw = (cw - 80.0).max(60.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                egui::Grid::new("lib-grid")
                    .num_columns(ncols)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        for (i, g) in libs.iter().enumerate() {
                    let is_sel = self.selected.as_ref() == Some(&g.id);
                    let dl_tex = self.dl_tex.clone();
                    let web_tex = self.web_tex.clone();
                    let has_apk = game_folder(&g.name).join("base.apk").exists();
                    let frac = self.card_frac(&g.id);
                    let frame = egui::Frame::new()
                        .fill(card)
                        .corner_radius(10u8)
                        .stroke(egui::Stroke::new(2.0_f32, if is_sel { accent } else { border }))
                        .inner_margin(0.0);
                    let fr = frame
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(cw, cw));
                            ui.set_max_size(egui::vec2(cw, cw));
                            // Portada 240x240 a sangre (recorte central, sin deformar).
                            // La ficha es la imagen: título, estado e iconos van superpuestos.
                            let r = if let Some(c) = self.covers.get(&g.id) {
                                ui.add(
                                    egui::Image::new(&c.tex)
                                        .uv(cover_uv(c.w, c.h))
                                        .corner_radius(10u8)
                                        .fit_to_exact_size(egui::vec2(cw, cw)),
                                )
                                .rect
                            } else {
                                let (rr, _) = ui.allocate_exact_size(
                                    egui::vec2(cw, cw),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rr,
                                    10.0,
                                    egui::Color32::from_rgb(0x1e, 0x1e, 0x1e),
                                );
                                let mut ph = ui.child_ui(
                                    rr,
                                    egui::Layout::centered_and_justified(
                                        egui::Direction::TopDown,
                                    ),
                                    None,
                                );
                                ph.label(egui::RichText::new("🎮").size(44.0));
                                rr
                            };
                            let (st, st_col) = if g.owned {
                                (
                                    tr(&lang, "Vinculado"),
                                    egui::Color32::from_rgb(0x7f, 0xd6, 0x7f),
                                )
                            } else if g.verified_at > 0 {
                                (
                                    tr(&lang, "No vinculado"),
                                    egui::Color32::from_rgb(0xc9, 0x6a, 0x5a),
                                )
                            } else {
                                (tr(&lang, "Sin comprobar"), egui::Color32::GRAY)
                            };
                            let st_txt = format!(
                                "{st}{}",
                                if has_apk {
                                    format!(" · {}", tr(&lang, "Descargado"))
                                } else {
                                    String::new()
                                }
                            );
                            // Barra inferior sobre la imagen: título + estado | iconos
                            let bar_h = 52.0;
                            let bar = egui::Rect::from_min_size(
                                egui::pos2(r.min.x, r.max.y - bar_h),
                                egui::vec2(r.width(), bar_h),
                            );
                            ui.painter().rect_filled(
                                bar,
                                egui::CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 },
                                egui::Color32::from_black_alpha(165),
                            );
                            let mut bar_ui = ui.child_ui(
                                bar.shrink(6.0),
                                egui::Layout::left_to_right(egui::Align::Min),
                                None,
                            );
                            bar_ui.vertical(|ui| {
                                ui.set_min_size(egui::vec2(tw, 0.0));
                                ui.set_max_size(egui::vec2(tw, 40.0));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&g.name)
                                            .strong()
                                            .size(13.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(st_txt).small().color(st_col),
                                    )
                                    .selectable(false),
                                );
                            });
                            // Iconos: solo dibujo (el clic se resuelve a mano en update)
                            let uv_full = egui::Rect::from_min_max(
                                egui::pos2(0.0, 0.0),
                                egui::pos2(1.0, 1.0),
                            );
                            let (web_ir, dl_ir) = card_hit_rects(r);
                            for (tex, ir) in
                                [(web_tex.as_ref(), web_ir), (dl_tex.as_ref(), dl_ir)]
                            {
                                if let Some(t) = tex {
                                    ui.painter().circle_filled(
                                        ir.center(),
                                        ir.height() / 2.0,
                                        egui::Color32::from_black_alpha(110),
                                    );
                                    ui.painter().image(
                                        t.id(),
                                        ir.shrink(5.0),
                                        uv_full,
                                        egui::Color32::WHITE,
                                    );
                                }
                            }
                            // Progreso: línea fina al pie de la ficha
                            if let Some(f) = frac {
                                let pw = r.width() * f.clamp(0.0, 1.0);
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(r.min.x, r.max.y - 4.0),
                                        egui::vec2(pw, 4.0),
                                    ),
                                    4u8,
                                    accent,
                                );
                            }
                            (web_ir, dl_ir)
                        });
                    let resp = fr.response;
                    self.hits
                        .push((g.id.clone(), resp.rect, fr.inner.0, fr.inner.1));
                    let _ = txt;
                            if i % ncols == ncols - 1 {
                                ui.end_row();
                            }
                        }
                    });
            });
        });
    }

    fn card_frac(&self, app_id: &str) -> Option<f32> {
        let jids = self.game_jobs.get(app_id)?;
        let snap = self.dm.snapshot();
        let jobs: Vec<_> = snap.iter().filter(|j| jids.contains(&j.id)).collect();
        if jobs.is_empty() {
            return None;
        }
        let tot: u64 = jobs.iter().map(|j| j.total).sum();
        if tot == 0 {
            return Some(0.0);
        }
        let done: u64 = jobs.iter().map(|j| j.done).sum();
        Some((done as f32 / tot as f32).clamp(0.0, 1.0))
    }

    fn ui_store(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang.clone();
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(tr(&lang, "Busca juegos de la Meta Quest Store…"))
                    .desired_width(400.0),
            );
            if ui.button(tr(&lang, "Buscar")).clicked()
                || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                self.do_search();
            }
            if self.searching {
                ui.spinner();
            }
        });
        let results = std::mem::take(&mut self.results);
        let mut action: Option<(usize, bool)> = None; // (idx, descargar?)
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, app) in results.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&app.name).strong());
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} · {}",
                                app.price, app.size, app.publisher
                            ))
                            .small(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr(&lang, "Abrir en la Store")).clicked() {
                            open_url(&app.store_url);
                        }
                        if ui.button(tr(&lang, "Descargar")).clicked() {
                            action = Some((i, true));
                        }
                        if ui.button(tr(&lang, "Añadir a Biblioteca")).clicked() {
                            action = Some((i, false));
                        }
                    });
                });
                ui.separator();
            }
        });
        if let Some((i, dl)) = action {
            if let Some(app) = results.into_iter().nth(i) {
                let g = store::Game {
                    id: app.id.clone(),
                    name: app.name.clone(),
                    package: app.package.clone(),
                    price: app.price.clone(),
                    size: app.size.clone(),
                    publisher: app.publisher.clone(),
                    store_url: app.store_url.clone(),
                    owned: false,
                    verified_at: 0,
                art_url: String::new(),
                            info: serde_json::json!({
                        "genres": app.genres, "image_url": app.image_url,
                    }),
                };
                store::library_add(&g);
                self.refresh_library();
                self.ensure_covers();
                if dl {
                    self.page = Page::Dl;
                    self.open_download_dialog(g);
                } else {
                    self.page = Page::Lib;
                }
            }
        } else {
            self.results = results;
        }
    }

    fn ui_downloads(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang.clone();
        ui.horizontal(|ui| {
            if ui.button(tr(&lang, "Abrir carpeta")).clicked() {
                std::fs::create_dir_all(store::download_dir()).ok();
                open_folder(&store::download_dir());
            }
        });
        let snap = self.dm.snapshot();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for j in &snap {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&j.name).strong());
                        let frac = if j.total > 0 {
                            j.done as f32 / j.total as f32
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                .desired_width(420.0)
                                .text(format!(
                                    "{:.1}% · {} / {}",
                                    frac * 100.0,
                                    mb(j.done),
                                    mb(j.total)
                                )),
                        );
                        ui.label(
                            egui::RichText::new(if j.status == JobStatus::Error {
                                format!("{}: {}", j.status.label(), j.error)
                            } else {
                                j.status.label().to_string()
                            })
                            .small(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr(&lang, "Cancelar")).clicked() {
                            self.dm.cancel(&j.id);
                        }
                        match j.status {
                            JobStatus::Downloading | JobStatus::Queued => {
                                if ui.button(tr(&lang, "Pausar")).clicked() {
                                    self.dm.pause(&j.id);
                                }
                            }
                            JobStatus::Paused | JobStatus::Error => {
                                if ui.button(tr(&lang, "Reanudar")).clicked() {
                                    DownloadManager::resume_with(&self.dm, &j.id);
                                }
                            }
                            _ => {}
                        }
                    });
                });
                ui.separator();
            }
            if snap.is_empty() {
                ui.label(tr(&lang, "Sin descargas. Descarga desde una ficha."));
            }
        });
    }
}

fn load_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .unwrap_or_else(|_| egui::IconData::default())
}

fn main() -> eframe::Result<()> {
    if std::env::args().any(|a| a == "--probe-browser") {
        match crate::browser::detect() {
            crate::browser::DefaultBrowser::Chromium(exe) => println!("PROBE Chromium {exe}"),
            crate::browser::DefaultBrowser::Firefox(exe) => println!("PROBE Firefox {exe}"),
            crate::browser::DefaultBrowser::Other => println!("PROBE Other"),
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([760.0, 540.0])
            .with_title("Quest Downloader - QD")
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "QD",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
