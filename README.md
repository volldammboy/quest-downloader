# Quest Downloader — QD

> 🇪🇸 ¿Prefieres español? Lee [README.es.md](README.es.md).

**Quest Downloader (QD)** is a portable Windows desktop app (Rust + egui) to manage your
Meta Quest library: link your Meta account, browse your owned games with covers, search
the Meta Quest Store catalog, and download games (APK + data/OBBs + optional language packs).

- Single portable executable, no installer, no console window.
- Three sections: **Library**, **Store**, **Downloads**.
- Interface in **English and Spanish** (auto-selected from Windows regional settings,
  switchable in the *Language* menu).
- Account tokens are protected with Windows DPAPI; sign-in runs locally via Microsoft Edge SSO.

## Installation

1. Get `QD.exe` (see *Build from source* below).
2. Place it in any folder and run it. That's it.
3. On first run it creates next to the executable:
   - `data/` — database (`data.db`), cached covers (`covers_v2/`) and account tokens.
   - `Downloads/` — one folder per game (`base.apk`, OBBs, metadata JSON, cover).

Requirements: Windows 10/11 64-bit. No runtime, no admin rights, no network drives needed.

## Usage

1. **Link your Meta account**: top-right button *Link Meta account*. A Meta sign-in page opens;
   complete it and the app syncs automatically. When linked, the button becomes
   *Unlink Meta account* (pressing it unlinks immediately).
2. **Library**: your owned games as uniform cards (cover fills the card). Click a card to
   select it (Details panel on the right). Icons overlaid on the cover:
   - ⬇ download arrow — opens the download dialog, or the download folder if already downloaded;
   - 🌐 globe — opens the game page in the Meta Store.
3. **Download dialog**: the base content goes as a single package (APK + data files, always
   included). Optional content is grouped **per language** (texts + audio in one pack each);
   languages already included in the base are not offered. Confirming starts the download and
   you stay in the current view; progress shows as a thin line at the bottom of the card.
4. **Store**: search the Meta Quest Store catalog, open results in the Store, add them to
   your library or download them directly.
5. **Downloads**: active downloads with progress, pause/resume/cancel, plus *Open folder*.
   Downloads resume with HTTP `Range` where the server allows it.

Keyboard: `F5` refresh library, `Delete` remove selected, `Ctrl+1/2/3` switch sections.

Notes:

- Only content owned by the linked account can be downloaded; anything else is rejected
  by Meta's servers.
- Covers are cached in `data/covers_v2/`; game metadata is stored in the local SQLite database.

## Build from source

Requirements: stable Rust (MSVC toolchain), Windows 10/11 SDK (MSVC + `rc.exe` for the icon),
then:

```bat
cargo build --release
```

The binary lands at `target\release\QD.exe`. The application icon comes from
`assets/icon.ico` (embedded at compile time).

## Project layout

```text
src/
  main.rs       UI (egui) + download dialog + i18n
  meta_auth.rs  Meta SSO sign-in (Edge), library, APK+data plan, DPAPI
  oculusdb.rs   Public store catalog + covers
  downloader.rs Resumable multi-threaded downloads
  store.rs      SQLite persistence (library, settings)
icons/          UI icons (download arrow, globe)
assets/         Application icon (.ico/.png)
```

## Disclaimer

Unofficial community project. Not affiliated with, endorsed by, or sponsored by Meta.
All game content belongs to its respective owners and is downloaded from Meta's own servers
under the linked account's entitlements.
