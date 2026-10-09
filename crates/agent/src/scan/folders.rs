//! Game folders the person names (`droidtop-agent folders add <path>`): a
//! folder with a program in it is a game; a folder without one holds games
//! (droidtop's own folder rule, SPEC 7h "A folder that holds games is a
//! container"), walked a few levels down. A GOG or Steam id the game's files
//! state (`goggame-<id>.info`, `steam_appid.txt`) gives it that store's key,
//! so it meets the same game elsewhere.

use std::fs;
use std::path::{Path, PathBuf};

use droidtop_agent_core::library::title_key;
use serde_json::Value;

use super::{pc_game, Found};
use crate::state::Settings;

const MAX_DEPTH: usize = 3;

/// Programs that come with a game but are not it.
const NOT_THE_GAME: &[&str] = &[
    "unins",
    "setup",
    "install",
    "crash",
    "redist",
    "vcredist",
    "dxsetup",
    "dotnet",
    "update",
    "uninstall",
    "notification_helper",
    "unitycrashhandler",
];

fn is_program(name: &str) -> bool {
    let lower = name.to_lowercase();
    let program = lower.ends_with(".exe")
        || lower.ends_with(".x86_64")
        || lower.ends_with(".x86")
        || lower.ends_with(".app")
        || lower.ends_with(".sh");
    program && !NOT_THE_GAME.iter().any(|n| lower.starts_with(n))
}

/// Whether a folder is a game: a program in it, or an engine's own layout.
fn is_game(dir: &Path) -> bool {
    let Ok(read) = fs::read_dir(dir) else { return false };
    let names: Vec<String> = read.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.iter().any(|n| is_program(n))
        || names.iter().any(|n| n == "renpy")
        || dir.join("game/script.rpyc").is_file()
        || dir.join("www/js/rpg_core.js").is_file()
}

/// The store key a game's own files state, when they state one.
fn store_key(dir: &Path) -> Option<String> {
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("goggame-") && name.ends_with(".info") {
                if let Some(id) = fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v["gameId"].as_str().map(str::to_string))
                {
                    return Some(format!("gog:{id}"));
                }
            }
        }
    }
    let id = fs::read_to_string(dir.join("steam_appid.txt")).ok()?;
    let id = id.trim();
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| format!("steam:{id}"))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Found>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut children: Vec<PathBuf> = read.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
    children.sort();
    for child in children {
        let Some(name) = child.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if name.starts_with('.') {
            continue;
        }
        if is_game(&child) {
            let key = store_key(&child).unwrap_or_else(|| title_key(&name));
            out.push(pc_game(key, name, Some(child), "folder", 0, None));
        } else if depth < MAX_DEPTH {
            walk(&child, depth + 1, out);
        }
    }
}

pub fn scan(settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = Vec::new();
    for root in &settings.game_folders {
        walk(root, 1, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_and_helpers_are_told_apart() {
        assert!(is_program("Game.exe"));
        assert!(is_program("game.x86_64"));
        assert!(!is_program("unins000.exe"));
        assert!(!is_program("UnityCrashHandler64.exe"));
        assert!(!is_program("readme.txt"));
    }

    #[test]
    fn containers_are_walked_and_store_ids_are_kept() {
        let root = std::env::temp_dir().join(format!("dta-folders-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("RenPy/Some Game/renpy")).unwrap();
        fs::create_dir_all(root.join("Other")).unwrap();
        fs::write(root.join("Other/Other.exe"), b"").unwrap();
        fs::write(root.join("Other/goggame-1207658924.info"), br#"{"gameId":"1207658924","name":"Other"}"#).unwrap();
        let settings = Settings { game_folders: vec![root.clone()], ..Default::default() };
        let found = scan(&settings).unwrap();
        let keys: Vec<&str> = found.iter().map(|f| f.game.key.as_str()).collect();
        assert_eq!(keys, vec!["gog:1207658924", "title:some game"]);
        let _ = fs::remove_dir_all(&root);
    }
}
