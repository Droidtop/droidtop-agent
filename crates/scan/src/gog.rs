//! GOG: the installer's registry keys on Windows
//! (`HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<id>`) and Heroic's
//! `gog_store/installed.json` (titles from its `library.json`) anywhere.
//! A GOG game copied into a game folder is still found by the folder scan
//! from its `goggame-<id>.info` file.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::{env_dir, home, pc_game, Found};
use crate::state::Settings;

#[cfg(windows)]
fn registry() -> Vec<Found> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    let mut out = Vec::new();
    for path in [r"SOFTWARE\WOW6432Node\GOG.com\Games", r"SOFTWARE\GOG.com\Games"] {
        let Ok(games) = winreg::RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) else { continue };
        for id in games.enum_keys().flatten() {
            let Ok(g) = games.open_subkey(&id) else { continue };
            // A DLC names the game it belongs to; it shares that game's folder.
            if g.get_value::<String, _>("dependsOn").is_ok_and(|d| !d.trim().is_empty()) {
                continue;
            }
            let title: String = g.get_value("gameName").or_else(|_| g.get_value("GAMENAME")).unwrap_or_else(|_| id.clone());
            let base = g.get_value::<String, _>("path").ok().map(PathBuf::from).filter(|p| p.is_dir());
            let version = g.get_value::<String, _>("ver").ok().filter(|v| !v.is_empty());
            out.push(pc_game(format!("gog:{id}"), title, base, "gog", 0, version));
        }
    }
    out
}

#[cfg(not(windows))]
fn registry() -> Vec<Found> {
    Vec::new()
}

fn heroic_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut out = vec![h.join(".config/heroic/gog_store"), h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/gog_store")];
    if let Some(appdata) = env_dir("APPDATA") {
        out.push(appdata.join(r"heroic\gog_store"));
    }
    out.push(h.join("Library/Application Support/heroic/gog_store"));
    out
}

fn heroic() -> Vec<Found> {
    let mut out = Vec::new();
    for dir in heroic_dirs() {
        let Ok(text) = fs::read_to_string(dir.join("installed.json")) else { continue };
        let Ok(installed) = serde_json::from_str::<Value>(&text) else { continue };
        let titles: BTreeMap<String, String> = fs::read_to_string(dir.join("library.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|lib| lib["games"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|g| Some((g["app_name"].as_str()?.to_string(), g["title"].as_str()?.to_string())))
            .collect();
        for g in installed["installed"].as_array().cloned().unwrap_or_default() {
            if g["is_dlc"].as_bool() == Some(true) {
                continue;
            }
            let Some(id) = g["appName"].as_str() else { continue };
            let title = titles.get(id).cloned().unwrap_or_else(|| id.to_string());
            let base = g["install_path"].as_str().map(PathBuf::from).filter(|p| p.is_dir());
            let version = g["version"].as_str().map(str::to_string);
            out.push(pc_game(format!("gog:{id}"), title, base, "heroic", 0, version));
        }
    }
    out
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = registry();
    out.extend(heroic());
    Ok(out)
}
