//! Flashpoint (BlueMaxima's web game archive), met in a game folder: one
//! launcher, plus each game it has downloaded into `Data/Games`, named from
//! the launcher's own database (`Data/flashpoint.sqlite`, opened read-only).
//! Its `FPSoftware` folder holds the players and browser plugins it runs
//! games with, which are no games of their own.

use std::path::Path;

use droidtop_agent_core::library::{Install, ScannedGame};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::engines::Listing;
use super::{pc_game, Found};

/// A downloaded game's file is `<game id>-<timestamp>.zip`; the id is a UUID.
fn game_id(file: &str) -> Option<&str> {
    let id = file.get(..36)?;
    let uuid = id.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    uuid.then_some(id)
}

/// The launcher and its downloaded games, when [dir] is a Flashpoint install.
pub fn at(dir: &Path, listing: &Listing) -> Option<Vec<Found>> {
    let version_file = listing.files.iter().find(|n| n.eq_ignore_ascii_case("version.txt"))?;
    if !listing.dirs.iter().any(|n| n.eq_ignore_ascii_case("FPSoftware")) {
        return None;
    }
    let version = std::fs::read_to_string(dir.join(version_file)).ok()?;
    let version = version.lines().next().unwrap_or_default().trim().to_string();
    if !version.contains("Flashpoint") {
        return None;
    }
    let mut out = vec![pc_game("title:flashpoint".into(), "Flashpoint".into(), Some(dir.to_path_buf()), "folder", 0, Some(version))];
    let games = dir.join("Data").join("Games");
    let Ok(read) = std::fs::read_dir(&games) else { return Some(out) };
    // Read-only: the launcher's database is never written, and a database
    // that cannot be opened only costs the titles.
    let db = Connection::open_with_flags(
        dir.join("Data").join("flashpoint.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok();
    let mut files: Vec<String> =
        read.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    for file in files {
        let Some(id) = game_id(&file) else { continue };
        let title = db
            .as_ref()
            .and_then(|db| {
                db.query_row("SELECT title FROM game WHERE id = ?1", [id], |row| row.get::<_, String>(0)).optional().ok().flatten()
            })
            .unwrap_or_else(|| id.to_string());
        let path = games.join(&file);
        out.push(Found {
            game: ScannedGame {
                key: format!("flashpoint:{id}"),
                title,
                platform: Some("pc".into()),
                install: Install {
                    installed: true,
                    path: Some(path.display().to_string()),
                    size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                    launcher: Some("flashpoint".into()),
                    ..Default::default()
                },
            },
            base: Some(dir.to_path_buf()),
            prefix: None,
            engine: None,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_games_are_named_by_their_id() {
        assert_eq!(game_id("1a394056-abac-9481-7121-1c89417a6666-1651458251057.zip"), Some("1a394056-abac-9481-7121-1c89417a6666"));
        assert_eq!(game_id("readme.txt"), None);
    }

    #[test]
    fn an_install_is_one_launcher_and_its_downloads() {
        let root = std::env::temp_dir().join(format!("dta-flashpoint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("FPSoftware/Flash")).unwrap();
        std::fs::create_dir_all(root.join("Data/Games")).unwrap();
        std::fs::write(root.join("version.txt"), "Flashpoint 14.0.3 Infinity - Kingfisher\n").unwrap();
        std::fs::write(root.join("Data/Games/1a394056-abac-9481-7121-1c89417a6666-1651458251057.zip"), b"zip").unwrap();
        let db = Connection::open(root.join("Data/flashpoint.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE game (id TEXT PRIMARY KEY, title TEXT); INSERT INTO game VALUES ('1a394056-abac-9481-7121-1c89417a6666', 'A Flash Game');")
            .unwrap();
        drop(db);
        let listing = super::super::engines::Detector::default().listing(&root);
        let found = at(&root, &listing).unwrap();
        let titles: Vec<(&str, &str)> = found.iter().map(|f| (f.game.key.as_str(), f.game.title.as_str())).collect();
        assert_eq!(titles, vec![("title:flashpoint", "Flashpoint"), ("flashpoint:1a394056-abac-9481-7121-1c89417a6666", "A Flash Game")]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
