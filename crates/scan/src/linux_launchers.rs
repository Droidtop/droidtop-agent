//! Linux launchers beside the stores: Bottles' library (Windows programs in
//! bottles) and Minigalaxy (GOG's Linux and Windows installers). Both only
//! exist on Linux, natively or as a Flatpak; elsewhere they find nothing.

use std::fs;
use std::path::{Path, PathBuf};

use droidtop_agent_core::library::title_key;

use super::{home, pc_game, prefixes, Found};
use crate::state::Settings;

/// Bottles' data folders: native and Flatpak.
fn bottles_dirs() -> Vec<PathBuf> {
    let h = home();
    vec![h.join(".local/share/bottles"), h.join(".var/app/com.usebottles.bottles/data/bottles")]
}

/// The programs the person put in Bottles' library (`library.yml`): each
/// names its bottle and the Windows path of its program, so its folder is
/// the program's folder inside the bottle and its saves are in the bottle.
pub fn bottles(dirs: &[PathBuf]) -> Vec<Found> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(text) = fs::read_to_string(dir.join("library.yml")) else { continue };
        let Ok(serde_yaml::Value::Mapping(entries)) = serde_yaml::from_str::<serde_yaml::Value>(&text) else { continue };
        for (_, entry) in entries {
            let Some(name) = entry["name"].as_str().filter(|n| !n.is_empty()) else { continue };
            let bottle = entry["bottle"]["path"].as_str().or_else(|| entry["bottle"]["name"].as_str()).unwrap_or_default();
            if bottle.is_empty() {
                continue;
            }
            let bottle_dir = dir.join("bottles").join(bottle);
            let prefix = prefixes::prefix_at(&bottle_dir);
            let base = entry["path"].as_str().and_then(|p| windows_folder(&bottle_dir.join("drive_c"), p));
            let mut found = pc_game(title_key(name), name.to_string(), base, "bottles", 0, None);
            found.prefix = prefix;
            out.push(found);
        }
    }
    out
}

/// The folder of a program a Windows path names (`C:\Games\X\x.exe`), inside
/// a prefix's `drive_c`.
fn windows_folder(drive_c: &Path, path: &str) -> Option<PathBuf> {
    let rest = path.strip_prefix("C:\\").or_else(|| path.strip_prefix("C:/")).or_else(|| path.strip_prefix("c:\\"))?;
    let mut folder = drive_c.to_path_buf();
    for part in rest.split(['\\', '/']).filter(|p| !p.is_empty()) {
        folder.push(part);
    }
    folder.pop();
    folder.is_dir().then_some(folder)
}

/// Minigalaxy's settings: native and Flatpak.
fn minigalaxy_configs() -> Vec<PathBuf> {
    let h = home();
    vec![h.join(".config/minigalaxy/config.json"), h.join(".var/app/io.github.sharkwouter.Minigalaxy/config/minigalaxy/config.json")]
}

/// The games in Minigalaxy's install folder (`install_dir`, `~/GOG Games` by
/// default). A GOG Linux install names itself in `gameinfo` (title, then
/// version); a Windows one in `goggame-<id>.info`, read by the folder rule.
pub fn minigalaxy(configs: &[PathBuf]) -> Vec<Found> {
    let mut roots: Vec<PathBuf> = configs
        .iter()
        .filter_map(|c| fs::read_to_string(c).ok())
        .filter_map(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter_map(|v| v["install_dir"].as_str().map(PathBuf::from))
        .collect();
    if roots.is_empty() && configs.iter().any(|c| c.is_file()) {
        roots.push(home().join("GOG Games"));
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(read) = fs::read_dir(&root) else { continue };
        let mut dirs: Vec<PathBuf> = read.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        for dir in dirs {
            let info = fs::read_to_string(dir.join("gameinfo")).unwrap_or_default();
            let mut lines = info.lines().map(str::trim).filter(|l| !l.is_empty());
            let title = lines
                .next()
                .map(str::to_string)
                .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let version = lines.next().map(str::to_string);
            let key = super::folders::store_key(&dir).unwrap_or_else(|| title_key(&title));
            let mut found = pc_game(key, title, Some(dir.clone()), "minigalaxy", 0, version);
            found.prefix = prefixes::prefix_at(&dir.join("prefix")).or_else(|| prefixes::prefix_at(&dir));
            out.push(found);
        }
    }
    out
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = bottles(&bottles_dirs());
    out.extend(minigalaxy(&minigalaxy_configs()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottles_library_entries_are_games_in_their_bottle() {
        let dir = std::env::temp_dir().join(format!("dtagent-bottles-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bottles/Gaming/drive_c/users/me")).unwrap();
        fs::create_dir_all(dir.join("bottles/Gaming/drive_c/Games/Tea Party")).unwrap();
        fs::write(
            dir.join("library.yml"),
            "a1b2:\n  id: a1b2\n  name: Tea Party\n  path: C:\\Games\\Tea Party\\tea.exe\n  bottle:\n    name: Gaming\n    path: Gaming\n",
        )
        .unwrap();
        let found = bottles(std::slice::from_ref(&dir));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].game.key, "title:tea party");
        assert_eq!(found[0].base.as_deref(), Some(dir.join("bottles/Gaming/drive_c/Games/Tea Party").as_path()));
        assert_eq!(found[0].prefix.as_ref().unwrap().user, "me");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn minigalaxy_games_name_themselves() {
        let dir = std::env::temp_dir().join(format!("dtagent-minigalaxy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("games/Celeste")).unwrap();
        fs::write(dir.join("games/Celeste/gameinfo"), "Celeste\n1.4.0.0\n").unwrap();
        fs::write(dir.join("config.json"), serde_json::json!({ "install_dir": dir.join("games").display().to_string() }).to_string())
            .unwrap();
        let found = minigalaxy(&[dir.join("config.json")]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].game.title.as_str(), found[0].game.install.version.as_deref()), ("Celeste", Some("1.4.0.0")));
        fs::remove_dir_all(&dir).unwrap();
    }
}
