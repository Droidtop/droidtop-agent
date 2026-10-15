//! The games a desktop manager tracks itself (Pythia), kept in the agent's
//! data folder (`manager-games.json`) so they stay this computer's games
//! whichever program serves: Pythia, or droidtop-agent after Pythia closed
//! (docs/DESIGN.md section 16). Every rescan joins them where the scanner did
//! not find the game by key or folder.
//!
//! A paired handheld can copy a game's folder (`game_pull`), so a folder is
//! taken only when it is a game's and nothing more: not a filesystem root,
//! not the home folder or a folder holding it, and not a folder holding the
//! agent's or windowcast's own files.

use std::io;
use std::path::{Path, PathBuf};

use droidtop_agent_core::library::ScannedGame;
use droidtop_agent_scan::Found;
use droidtop_agent_store::{read_json, write_json, Dirs};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::events::Events;

/// One game a desktop manager tracks: the library's record of it, and its folder.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManagerGame {
    #[serde(flatten)]
    pub game: ScannedGame,
    #[serde(default)]
    pub base: Option<PathBuf>,
}

/// What `manager-games.json` holds: which manager, and its games.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ManagerGames {
    pub manager: String,
    pub games: Vec<ManagerGame>,
}

impl ManagerGames {
    pub fn load(dirs: &Dirs) -> ManagerGames {
        read_json(&dirs.manager_games()).unwrap_or_default()
    }

    pub fn save(&self, dirs: &Dirs) -> io::Result<()> {
        write_json(&dirs.manager_games(), self)
    }

    /// The games a scan joins: each with a folder that passes [`folder_allowed`]
    /// or with none. The others are left out and said once per scan.
    pub fn usable(&self, dirs: &Dirs, events: &Events) -> Vec<Found> {
        self.games
            .iter()
            .filter(|g| match g.base.as_deref().map(|b| folder_allowed(b, dirs)) {
                Some(Err(why)) => {
                    events.warn(
                        "manager",
                        format!("Left out {} from {}: {why}", g.game.title, self.manager),
                        json!({ "title": g.game.title, "key": g.game.key, "manager": self.manager }),
                    );
                    false
                }
                _ => true,
            })
            .map(|g| Found { game: g.game.clone(), base: g.base.clone(), prefix: None, engine: None })
            .collect()
    }
}

/// Whether [`folder`] may be a manager game's folder (see the module's comment).
pub fn folder_allowed(folder: &Path, dirs: &Dirs) -> Result<(), String> {
    let canonical = folder.canonicalize().map_err(|_| format!("{} is not a folder here", folder.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    if canonical.parent().is_none() {
        return Err(format!("{} is a whole drive", folder.display()));
    }
    let holds = |inner: &Path| inner.canonicalize().is_ok_and(|i| i.starts_with(&canonical));
    if dirs::home_dir().is_some_and(|home| holds(&home)) {
        return Err(format!("{} holds your home folder", folder.display()));
    }
    if [&dirs.config, &dirs.data, &dirs.computer].into_iter().any(|d| holds(d)) {
        return Err(format!("{} holds the agent's own files", folder.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(key: &str, base: Option<PathBuf>) -> ManagerGame {
        ManagerGame {
            game: ScannedGame { key: key.into(), title: key.into(), platform: Some("pc".into()), install: Default::default() },
            base,
        }
    }

    #[test]
    fn a_manager_game_is_kept_only_with_a_folder_that_is_a_games() {
        let root = std::env::temp_dir().join(format!("dta-manager-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs { config: root.join("agent"), data: root.join("agent/data"), computer: root.join("agent/host") };
        let folder = root.join("games/Some Game");
        for d in [&dirs.config, &dirs.data, &dirs.computer, &folder] {
            std::fs::create_dir_all(d).unwrap();
        }
        let list = ManagerGames {
            manager: "Pythia".into(),
            games: vec![
                game("title:some game", Some(folder.clone())),
                game("title:no folder", None),
                game("title:gone", Some(root.join("games/Gone"))),
                game("title:agent", Some(root.join("agent"))),
                game("title:home", dirs::home_dir()),
            ],
        };
        list.save(&dirs).unwrap();
        let events = Events::default();
        let kept: Vec<String> = ManagerGames::load(&dirs).usable(&dirs, &events).into_iter().map(|f| f.game.key).collect();
        assert_eq!(kept, ["title:some game", "title:no folder"]);
        let left_out: Vec<String> = events.since(0).iter().map(|e| e.detail["key"].as_str().unwrap().to_string()).collect();
        assert_eq!(left_out, ["title:gone", "title:agent", "title:home"]);
        assert!(folder_allowed(&root.join("games"), &dirs).is_ok());
        assert!(folder_allowed(&root, &dirs).is_err(), "it holds the agent's files");
        let _ = std::fs::remove_dir_all(&root);
    }
}
