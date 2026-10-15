//! Amazon Games: the app's install database on Windows
//! (`%LOCALAPPDATA%\Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite`) and
//! Heroic's nile files anywhere.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::{columns, env_dir, home, pc_game, DbCopy, Found};
use crate::state::Settings;

fn app_database() -> Result<Vec<Found>, String> {
    let Some(local) = env_dir("LOCALAPPDATA") else { return Ok(Vec::new()) };
    let path = local.join(r"Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let db = DbCopy::open(&path)?;
    let cols = columns(&db.conn, "DbSet");
    for needed in ["Id", "InstallDirectory", "ProductTitle"] {
        if !cols.iter().any(|c| c == needed) {
            return Err(format!("its install database has no {needed} column"));
        }
    }
    let installed = if cols.iter().any(|c| c == "Installed") { "WHERE Installed = 1" } else { "" };
    let mut stmt =
        db.conn.prepare(&format!("SELECT Id, ProductTitle, InstallDirectory FROM DbSet {installed}")).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))
        .map_err(|e| e.to_string())?;
    Ok(rows
        .flatten()
        .map(|(id, title, dir)| {
            let base = dir.map(PathBuf::from).filter(|p| p.is_dir());
            pc_game(format!("amazon:{id}"), title.unwrap_or_else(|| id.clone()), base, "amazon", 0, None)
        })
        .collect())
}

fn nile_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut out =
        vec![h.join(".config/heroic/nile_config/nile"), h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/nile_config/nile")];
    if let Some(appdata) = env_dir("APPDATA") {
        out.push(appdata.join(r"heroic\nile_config\nile"));
    }
    out.push(h.join("Library/Application Support/heroic/nile_config/nile"));
    out
}

fn nile() -> Vec<Found> {
    let mut out = Vec::new();
    for dir in nile_dirs() {
        let Ok(text) = fs::read_to_string(dir.join("installed.json")) else { continue };
        let Ok(Value::Array(installed)) = serde_json::from_str::<Value>(&text) else { continue };
        let titles: BTreeMap<String, String> = fs::read_to_string(dir.join("library.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|g| Some((g["id"].as_str()?.to_string(), g["product"]["title"].as_str()?.to_string())))
            .collect();
        for g in installed {
            let Some(id) = g["id"].as_str() else { continue };
            let title = titles.get(id).cloned().unwrap_or_else(|| id.to_string());
            let base = g["path"].as_str().map(PathBuf::from).filter(|p| p.is_dir());
            let size = g["size"].as_u64().unwrap_or(0);
            let version = g["version"].as_str().map(str::to_string);
            out.push(pc_game(format!("amazon:{id}"), title, base, "heroic", size, version));
        }
    }
    out
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = app_database()?;
    out.extend(nile());
    Ok(out)
}
