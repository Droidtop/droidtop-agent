//! Epic: the launcher's install manifests on Windows
//! (`%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item`) and, on any
//! system, legendary's `installed.json`, which Heroic keeps too.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::{env_dir, home, pc_game, Found};
use crate::state::Settings;

fn launcher_manifests() -> Vec<Found> {
    let Some(data) = env_dir("ProgramData") else { return Vec::new() };
    let dir = data.join(r"Epic\EpicGamesLauncher\Data\Manifests");
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in read.flatten() {
        if entry.path().extension().is_none_or(|e| e != "item") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else { continue };
        let Ok(item) = serde_json::from_str::<Value>(&text) else { continue };
        let app = item["AppName"].as_str().unwrap_or_default();
        let main = item["MainGameAppName"].as_str().unwrap_or(app);
        if app.is_empty() || app != main || item["bIsIncompleteInstall"].as_bool() == Some(true) {
            continue;
        }
        let title = item["DisplayName"].as_str().unwrap_or(app).to_string();
        let base = item["InstallLocation"].as_str().map(PathBuf::from).filter(|p| p.is_dir());
        let size = item["InstallSize"].as_u64().unwrap_or(0);
        let version = item["AppVersionString"].as_str().map(str::to_string);
        out.push(pc_game(format!("epic:{app}"), title, base, "epic", size, version));
    }
    out
}

/// legendary's own folder, and Heroic's copy of it on each system.
fn legendary_files() -> Vec<PathBuf> {
    let h = home();
    let mut out = vec![
        h.join(".config/legendary/installed.json"),
        h.join(".config/heroic/legendaryConfig/legendary/installed.json"),
        h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/legendaryConfig/legendary/installed.json"),
        h.join("Library/Application Support/heroic/legendaryConfig/legendary/installed.json"),
    ];
    if let Some(appdata) = env_dir("APPDATA") {
        out.push(appdata.join(r"heroic\legendaryConfig\legendary\installed.json"));
    }
    out
}

fn legendary() -> Vec<Found> {
    let mut out = Vec::new();
    for file in legendary_files() {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        let Ok(Value::Object(games)) = serde_json::from_str::<Value>(&text) else { continue };
        for (app, g) in games {
            if g["is_dlc"].as_bool() == Some(true) {
                continue;
            }
            let title = g["title"].as_str().unwrap_or(&app).to_string();
            let base = g["install_path"].as_str().map(PathBuf::from).filter(|p| p.is_dir());
            let size = g["install_size"].as_u64().unwrap_or(0);
            let version = g["version"].as_str().map(str::to_string);
            out.push(pc_game(format!("epic:{app}"), title, base, "heroic", size, version));
        }
    }
    out
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = launcher_manifests();
    out.extend(legendary());
    Ok(out)
}
