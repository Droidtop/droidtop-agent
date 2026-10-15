//! Other programs' state on this computer that droidtop can sync with or
//! learn from: F95Checker's database (synced through a plugin's context
//! adapter), Playnite's library,
//! Ludusavi's own settings, launchers and emulators with their save folders.
//! The scan only says what is there; the context adapters and the save
//! catalog do the reading.

use std::path::PathBuf;

use super::{env_dir, home, Program};
use crate::state::Settings;

fn app(id: &str, name: &str, path: PathBuf, note: &str) -> Option<Program> {
    path.exists().then(|| Program { id: id.into(), name: name.into(), path, note: note.into() })
}

/// F95Checker's data folder, the same place F95Checker itself uses
/// (its `modules/globals.py`).
pub fn f95checker_dir() -> PathBuf {
    let h = home();
    if cfg!(windows) {
        h.join("AppData/Roaming/f95checker")
    } else if cfg!(target_os = "macos") {
        h.join("Library/Application Support/f95checker")
    } else {
        h.join(".config/f95checker")
    }
}

/// Ludusavi's settings file, where the person's own custom games are.
pub fn ludusavi_config() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("ludusavi/config.yaml")
}

pub fn scan(_settings: &Settings) -> Vec<Program> {
    let h = home();
    let appdata = env_dir("APPDATA").unwrap_or_else(|| h.join("AppData/Roaming"));
    let docs = dirs::document_dir().unwrap_or_else(|| h.join("Documents"));
    let config = dirs::config_dir().unwrap_or_else(|| h.join(".config"));
    let data = dirs::data_dir().unwrap_or_else(|| h.join(".local/share"));
    let candidates = [
        app(
            "f95checker",
            "F95Checker",
            f95checker_dir().join("db.sqlite3"),
            "watched threads; synced through the F95 plugin's context adapter",
        ),
        app("playnite", "Playnite", appdata.join("Playnite/library"), "library in LiteDB; detected only"),
        app("ludusavi", "Ludusavi", ludusavi_config(), "custom games and save paths; used for save locations"),
        app("heroic", "Heroic", config.join("heroic"), "Epic, GOG and Amazon installs; read by the scan"),
        app("lutris", "Lutris", data.join("lutris/pga.db"), "installed games; read by the scan"),
        app("es-de", "ES-DE", h.join("ES-DE"), "ROM folder; read by the scan"),
        app(
            "retroarch",
            "RetroArch",
            if cfg!(windows) { appdata.join("RetroArch") } else { config.join("retroarch") },
            "emulator; saves and states",
        ),
        app("dolphin", "Dolphin", if cfg!(windows) { docs.join("Dolphin Emulator") } else { data.join("dolphin-emu") }, "emulator; saves"),
        app("pcsx2", "PCSX2", if cfg!(windows) { docs.join("PCSX2") } else { config.join("PCSX2") }, "emulator; memory cards"),
        app(
            "duckstation",
            "DuckStation",
            if cfg!(windows) { docs.join("DuckStation") } else { data.join("duckstation") },
            "emulator; memory cards",
        ),
        app("ppsspp", "PPSSPP", if cfg!(windows) { docs.join("PPSSPP") } else { config.join("ppsspp") }, "emulator; saves"),
        app("rpcs3", "RPCS3", if cfg!(windows) { appdata.join("rpcs3") } else { config.join("rpcs3") }, "emulator; saves"),
        app("cemu", "Cemu", if cfg!(windows) { appdata.join("Cemu") } else { data.join("Cemu") }, "emulator; saves"),
        app("ryujinx", "Ryujinx", if cfg!(windows) { appdata.join("Ryujinx") } else { config.join("Ryujinx") }, "emulator; saves"),
    ];
    candidates.into_iter().flatten().collect()
}
