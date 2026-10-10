//! Steam: the client's library folders (`steamapps/libraryfolders.vdf`) and
//! each installed app's `appmanifest_<id>.acf`. Tools Steam installs beside
//! games (Proton, the runtimes, the redistributables) are left out. On Linux
//! a game that runs under Proton gets its prefix, where its Windows saves are.

use std::fs;
use std::path::{Path, PathBuf};

use super::{env_dir, first_dir, home, pc_game, vdf, Found, Prefix};
use crate::state::Settings;

/// Where the Steam client is on this computer.
pub fn roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        if let Ok(key) = winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam") {
            if let Ok(path) = key.get_value::<String, _>("SteamPath") {
                out.push(PathBuf::from(path));
            }
        }
        if let Some(pf) = env_dir("ProgramFiles(x86)") {
            out.push(pf.join("Steam"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let h = home();
        out.push(h.join(".steam/steam"));
        out.push(h.join(".local/share/Steam"));
        out.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        out.push(h.join("snap/steam/common/.local/share/Steam"));
        if let Some(d) = env_dir("XDG_DATA_HOME") {
            out.push(d.join("Steam"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(home().join("Library/Application Support/Steam"));
    }
    let _ = (env_dir, home);
    let mut seen = Vec::new();
    for p in out {
        let canonical = p.canonicalize().unwrap_or(p);
        if canonical.join("steamapps").is_dir() && !seen.contains(&canonical) {
            seen.push(canonical);
        }
    }
    seen
}

/// Every library folder the client knows, itself first.
fn libraries(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let Ok(text) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else { return out };
    let parsed = vdf::parse(&text);
    let Some(folders) = parsed.get("libraryfolders") else { return out };
    for (key, value) in folders.children() {
        let path = match value {
            vdf::Value::Block(_) => value.text("path").map(PathBuf::from),
            // The old format: "1" "D:\\SteamLibrary".
            vdf::Value::Text(t) if key.chars().all(|c| c.is_ascii_digit()) => Some(PathBuf::from(t)),
            vdf::Value::Text(_) => None,
        };
        if let Some(p) = path {
            let p = p.canonicalize().unwrap_or(p);
            if p.join("steamapps").is_dir() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Not games: the tools Steam installs as apps.
fn is_tool(id: &str, name: &str) -> bool {
    matches!(id, "228980" | "1070560" | "1391110" | "1628350" | "1493710")
        || name.starts_with("Proton ")
        || name.starts_with("Steam Linux Runtime")
        || name.starts_with("Steamworks Common Redistributables")
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = Vec::new();
    for root in roots() {
        for lib in libraries(&root) {
            let apps = lib.join("steamapps");
            let Ok(read) = fs::read_dir(&apps) else { continue };
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(entry.path()) else { continue };
                let parsed = vdf::parse(&text);
                let Some(app) = parsed.get("AppState") else { continue };
                let (Some(id), Some(title)) = (app.text("appid"), app.text("name")) else { continue };
                if is_tool(id, title) {
                    continue;
                }
                let flags: u32 = app.text("StateFlags").and_then(|f| f.parse().ok()).unwrap_or(0);
                if flags & 4 == 0 {
                    continue;
                }
                let base = app.text("installdir").map(|d| apps.join("common").join(d)).filter(|p| p.is_dir());
                let size = app.text("SizeOnDisk").and_then(|s| s.parse().ok()).unwrap_or(0);
                let version = app.text("buildid").map(|b| format!("build {b}"));
                let mut found = pc_game(format!("steam:{id}"), title.to_string(), base, "steam", size, version);
                found.prefix = proton_prefix(&apps, id);
                out.push(found);
            }
        }
    }
    Ok(out)
}

/// The Proton prefix Steam made for an app, when there is one.
fn proton_prefix(apps: &Path, id: &str) -> Option<Prefix> {
    let drive_c = first_dir([apps.join("compatdata").join(id).join("pfx/drive_c")])?;
    Some(Prefix { drive_c, user: "steamuser".into() })
}

#[cfg(test)]
mod tests {
    #[test]
    fn tools_are_not_games() {
        assert!(super::is_tool("228980", "Steamworks Common Redistributables"));
        assert!(super::is_tool("1", "Proton 9.0"));
        assert!(!super::is_tool("440", "Team Fortress 2"));
    }
}
