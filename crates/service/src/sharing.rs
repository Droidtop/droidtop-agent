//! What each paired handheld may watch when it streams from this computer
//! (docs/DESIGN.md section 22, Droidtop/tracker#116): every window, as
//! before, or only the windows of the game it starts here. Kept in this
//! agent's own `stream-sharing.json` beside its settings, by device id.
//!
//! "Every window" is the default: a handheld also streams the computer's
//! other applications (droidtop's Apps), which reach windowcast directly
//! and would be refused under a limit to one game.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What one handheld may watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sharing {
    /// Every window this computer shows.
    #[default]
    All,
    /// Only the windows of the game it starts here, while it watches that game.
    Games,
}

impl Sharing {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "all" => Some(Sharing::All),
            "games" => Some(Sharing::Games),
            _ => None,
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Sharing::All => "every window",
            Sharing::Games => "only the games it starts",
        }
    }
}

fn file(config: &Path) -> PathBuf {
    config.join("stream-sharing.json")
}

fn all(config: &Path) -> BTreeMap<String, Sharing> {
    std::fs::read_to_string(file(config)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// What `device` (its id in hex) may watch.
pub fn of(config: &Path, device: &str) -> Sharing {
    all(config).get(device).copied().unwrap_or_default()
}

/// Sets what `device` may watch; written whole, through a temporary file.
pub fn set(config: &Path, device: &str, sharing: Sharing) -> std::io::Result<()> {
    let mut map = all(config);
    if sharing == Sharing::All {
        map.remove(device);
    } else {
        map.insert(device.to_string(), sharing);
    }
    std::fs::create_dir_all(config)?;
    let tmp = file(config).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&map).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, file(config))
}

/// The window globs that are a game's: its title anywhere in a window's
/// title, and each program at the top of its install folder by name (a
/// Windows window's app id is its program's file name).
pub fn game_globs(title: &str, base: Option<&Path>) -> Vec<String> {
    let mut globs = Vec::new();
    let title = title.trim();
    if !title.is_empty() {
        globs.push(format!("*{title}*"));
    }
    if let Some(entries) = base.and_then(|b| std::fs::read_dir(b).ok()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.to_ascii_lowercase().ends_with(".exe") {
                globs.push(name);
            }
        }
    }
    globs.sort();
    globs.dedup();
    globs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_is_the_default_and_a_choice_is_kept() {
        let dir = std::env::temp_dir().join(format!("agent-sharing-{}", std::process::id()));
        assert_eq!(of(&dir, "ab"), Sharing::All);
        set(&dir, "ab", Sharing::Games).unwrap();
        assert_eq!(of(&dir, "ab"), Sharing::Games);
        assert_eq!(of(&dir, "cd"), Sharing::All);
        set(&dir, "ab", Sharing::All).unwrap();
        assert_eq!(of(&dir, "ab"), Sharing::All);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_games_windows_are_its_title_and_its_programs() {
        let dir = std::env::temp_dir().join(format!("agent-game-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("Sandustry.exe"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        assert_eq!(game_globs(" Sandustry ", Some(&dir)), vec!["*Sandustry*".to_string(), "Sandustry.exe".to_string()]);
        assert_eq!(game_globs("", None), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
