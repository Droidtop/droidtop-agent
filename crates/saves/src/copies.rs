//! The copies of games' saves kept on this computer (docs/DESIGN.md section
//! 9): per game, the newest copy from each device (`copies/<device>/`), then
//! the older ones, newest first, each dated folder with the `device.txt` of
//! the device it came from.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use droidtop_agent_core::savesync;
use serde::Serialize;

/// One kept copy of a game's saves.
#[derive(Debug, Clone, Serialize)]
pub struct Kept {
    /// The device it came from.
    pub device: String,
    /// When: a UTC stamp such as `20261008T224500Z`.
    pub stamp: String,
    pub files: usize,
    pub path: PathBuf,
    /// The newest copy from a device, rather than an older one it replaced.
    pub newest: bool,
    /// The person said they had looked at it ([`mark_reviewed`]).
    pub reviewed: bool,
    /// What `reviewed.txt` calls it.
    pub name: String,
}

/// A game's kept copies. [`key`] is None when the library no longer has the
/// game; [`title`] is None when nothing names it (no library entry and no
/// `title.txt` beside its copies).
#[derive(Debug, Clone, Serialize)]
pub struct GameCopies {
    pub key: Option<String>,
    pub title: Option<String>,
    pub dir: PathBuf,
    pub copies: Vec<Kept>,
}

impl GameCopies {
    /// Whether an older copy kept after both sides changed waits for the person.
    pub fn to_review(&self) -> bool {
        self.copies.iter().any(|c| !c.newest && !c.reviewed)
    }
}

/// Every game's kept copies under [`root`] (the agent's archive folder).
/// [`titles`] names each archive folder's game ([`crate::library_titles`]).
pub fn kept(root: &Path, titles: &BTreeMap<String, (String, String)>) -> Vec<GameCopies> {
    let mut out = Vec::new();
    let mut games: Vec<PathBuf> =
        std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    games.sort();
    for dir in games {
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let reviewed: Vec<String> =
            std::fs::read_to_string(dir.join("reviewed.txt")).unwrap_or_default().lines().map(str::to_string).collect();
        let mut copies = Vec::new();
        let mut newest: Vec<PathBuf> = std::fs::read_dir(dir.join("copies")).into_iter().flatten().flatten().map(|e| e.path()).collect();
        newest.sort();
        for copy in newest {
            let device = copy.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let stamp = std::fs::metadata(&copy).and_then(|m| m.modified()).map(savesync::utc_stamp).unwrap_or_default();
            copies.push(Kept {
                device: device.clone(),
                stamp,
                files: walk_count(&copy, 0),
                path: copy,
                newest: true,
                reviewed: true,
                name: device,
            });
        }
        for older in savesync::archived(&dir).into_iter().rev() {
            let stamp = older.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let device = std::fs::read_to_string(older.join("device.txt")).unwrap_or_default();
            let files = walk_count(&older, 0).saturating_sub(usize::from(!device.is_empty()));
            copies.push(Kept {
                device: device.trim().to_string(),
                stamp: stamp.clone(),
                files,
                path: older,
                newest: false,
                reviewed: reviewed.contains(&stamp),
                name: stamp,
            });
        }
        if copies.is_empty() {
            continue;
        }
        let (key, title) = match titles.get(&folder) {
            Some((key, title)) => (Some(key.clone()), Some(title.clone())),
            None => {
                let kept = std::fs::read_to_string(dir.join("title.txt")).unwrap_or_default();
                (None, Some(kept.trim().to_string()).filter(|t| !t.is_empty()))
            }
        };
        out.push(GameCopies { key, title, dir, copies });
    }
    out
}

/// The person looked at [`game`]'s older copies and keeps what is here: they
/// stop being marked as waiting for them.
pub fn mark_reviewed(game: &GameCopies) -> io::Result<()> {
    let names: Vec<String> = game.copies.iter().filter(|c| !c.newest).map(|c| c.name.clone()).collect();
    std::fs::write(game.dir.join("reviewed.txt"), names.join("\n"))
}

fn walk_count(dir: &Path, depth: usize) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| if e.path().is_dir() && depth < 8 { walk_count(&e.path(), depth + 1) } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_lists_each_devices_copy_then_older_ones() {
        let root = std::env::temp_dir().join(format!("dtagent-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("steam_440");
        let save = root.join("save.dat");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&save, b"x").unwrap();
        for device in ["DESKTOP", "DESKTOP", "Retroid Pocket 5"] {
            savesync::keep_copy(std::iter::once(("<base>/save.dat".to_string(), save.clone())), &game, device).unwrap();
        }
        let titles = BTreeMap::from([("steam_440".to_string(), ("steam:440".to_string(), "Team Fortress 2".to_string()))]);
        let list = kept(&root, &titles);
        // One game, named by its title and not by its folder.
        assert_eq!((list.len(), list[0].title.as_deref(), list[0].key.as_deref()), (1, Some("Team Fortress 2"), Some("steam:440")));
        let seen: Vec<(&str, bool)> = list[0].copies.iter().map(|a| (a.device.as_str(), a.newest)).collect();
        assert_eq!(&seen[..2], &[("DESKTOP", true), ("Retroid Pocket 5", true)]);
        assert_eq!(seen[2], ("DESKTOP", false));
        assert!(list[0].copies.iter().all(|a| a.files == 1));
        assert!(list[0].copies[2].stamp.ends_with('Z'));
        assert!(list[0].to_review());
        mark_reviewed(&list[0]).unwrap();
        assert!(!kept(&root, &titles)[0].to_review());
        // A game the library does not list is named by the title kept beside its copies, never by its key.
        let other = root.join("folder_CUSTOM_GAME_7");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("title.txt"), "Custom Thing").unwrap();
        savesync::keep_copy(std::iter::once(("<base>/save.dat".to_string(), save.clone())), &other, "DESKTOP").unwrap();
        let list = kept(&root, &titles);
        let named: Vec<(Option<&str>, Option<&str>)> = list.iter().map(|g| (g.title.as_deref(), g.key.as_deref())).collect();
        assert!(
            named.contains(&(Some("Custom Thing"), None)) && named.contains(&(Some("Team Fortress 2"), Some("steam:440"))),
            "{named:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
