//! The computer's scanner (docs/DESIGN.md section 11): what is installed,
//! from the files launchers and stores leave behind. No store APIs, no
//! network. Each source is read on its own, so one that fails (a schema
//! change, an unreadable file) costs its games and not the whole scan.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use droidtop_agent_core::library::{Install, ScannedGame};
use serde::Serialize;

use crate::state::{Dirs, Settings};

mod amazon;
pub mod apps;
mod battlenet;
mod epic;
mod folders;
mod gog;
mod itch;
mod lutris;
mod roms;
mod steam;
pub mod vdf;

/// A Wine or Proton prefix a game runs in on this computer.
#[derive(Debug, Clone, Serialize)]
pub struct Prefix {
    /// The prefix's `drive_c`.
    pub drive_c: PathBuf,
    /// The Windows user inside it.
    pub user: String,
}

/// One game the scan found, with what save lookups need.
#[derive(Debug, Clone, Serialize)]
pub struct Found {
    pub game: ScannedGame,
    /// The game's folder.
    pub base: Option<PathBuf>,
    /// Where its Windows saves are when it runs under Proton or Wine here.
    pub prefix: Option<Prefix>,
}

/// Another program's state the agent can read: launchers, emulators,
/// F95Checker, Playnite.
#[derive(Debug, Clone, Serialize)]
pub struct App {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Scan {
    pub games: Vec<Found>,
    pub apps: Vec<App>,
    /// Sources that failed, and why: shown by `scan`, never fatal.
    pub problems: Vec<String>,
}

impl Scan {
    pub fn find(&self, key: &str) -> Option<&Found> {
        self.games.iter().find(|f| f.game.key == key)
    }

    pub fn find_title(&self, title: &str) -> Option<&Found> {
        let wanted = droidtop_agent_core::library::title_key(title);
        self.games.iter().find(|f| droidtop_agent_core::library::title_key(&f.game.title) == wanted)
    }
}

/// One source of games: what it found, or why it could not read.
type Source = fn(&Settings) -> Result<Vec<Found>, String>;

pub fn scan(settings: &Settings, dirs: &Dirs) -> Scan {
    let mut out = Scan::default();
    let sources: Vec<(&str, Source)> = vec![
        ("Steam", steam::scan),
        ("Epic", epic::scan),
        ("GOG", gog::scan),
        ("Amazon", amazon::scan),
        ("itch", itch::scan),
        ("Battle.net", battlenet::scan),
        ("Lutris", lutris::scan),
        ("game folders", folders::scan),
        ("ROM folders", roms::scan),
    ];
    let mut by_key: BTreeMap<String, Found> = BTreeMap::new();
    for (name, source) in sources {
        match source(settings) {
            Ok(found) => {
                for f in found {
                    // The first source to name a game keeps it: a store's own
                    // record beats a folder walk of the same install.
                    by_key.entry(f.game.key.clone()).or_insert(f);
                }
            }
            Err(e) => out.problems.push(format!("{name}: {e}")),
        }
    }
    out.games = by_key.into_values().collect();
    out.apps = apps::scan(settings);
    let _ = dirs;
    out
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// An environment variable as a folder, when it is set.
pub fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The first of [`candidates`] that is a folder.
pub fn first_dir(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_dir())
}

/// A PC game a store or launcher records as installed.
pub fn pc_game(key: String, title: String, base: Option<PathBuf>, launcher: &str, size: u64, version: Option<String>) -> Found {
    Found {
        game: ScannedGame {
            key,
            title,
            platform: Some("pc".into()),
            install: Install {
                installed: true,
                path: base.as_ref().map(|p| p.display().to_string()),
                size,
                version,
                launcher: Some(launcher.to_string()),
                ..Default::default()
            },
        },
        base,
        prefix: None,
    }
}

static COPIES: AtomicU64 = AtomicU64::new(0);

/// A copy of a launcher's SQLite database (with its write-ahead log) in a
/// temporary folder, removed when dropped: reading a copy never takes a lock
/// the launcher could trip over, and sees changes still in its log.
pub struct DbCopy {
    dir: PathBuf,
    pub conn: rusqlite::Connection,
}

impl DbCopy {
    pub fn open(path: &Path) -> Result<DbCopy, String> {
        if !path.is_file() {
            return Err(format!("{} is not there", path.display()));
        }
        let dir = std::env::temp_dir().join(format!("droidtop-agent-db-{}-{}", std::process::id(), COPIES.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let target = dir.join("db.sqlite");
        fs::copy(path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", path.display()));
            if side.is_file() {
                let _ = fs::copy(&side, PathBuf::from(format!("{}{suffix}", target.display())));
            }
        }
        let conn = rusqlite::Connection::open(&target).map_err(|e| e.to_string())?;
        Ok(DbCopy { dir, conn })
    }
}

impl Drop for DbCopy {
    fn drop(&mut self) {
        // Only ever the folder made above, under the system's temp folder.
        if self.dir.starts_with(std::env::temp_dir())
            && self.dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with("droidtop-agent-db-"))
        {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// The columns a table has, so a query can adapt to a launcher's schema.
pub fn columns(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else { return Vec::new() };
    stmt.query_map([], |row| row.get::<_, String>(1)).map(|rows| rows.flatten().collect()).unwrap_or_default()
}
