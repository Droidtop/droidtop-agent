//! The computer's scanner (docs/DESIGN.md section 11): what is installed,
//! from the files launchers and stores leave behind. No store APIs, no
//! network. Each source is read on its own, so one that fails (a schema
//! change, an unreadable file) costs its games and not the whole scan.
//!
//! A crate of its own (docs/DESIGN.md section 13): it needs only the
//! folders it is told about ([`Options`]), never the agent's state, so the
//! running agent and a desktop manager (Pythia) call the same scanner.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use droidtop_agent_core::library::{Install, ScannedGame};
use serde::Serialize;

/// What a scan looks at besides the stores and launchers it finds itself:
/// the person's game and ROM folders, and what they switched off.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Folders whose subfolders are games.
    pub game_folders: Vec<PathBuf>,
    /// Folders laid out by ES-DE system name.
    pub rom_folders: Vec<PathBuf>,
    /// Game folders switched off: listed, but not walked.
    pub disabled_folders: Vec<PathBuf>,
    /// Launchers and stores switched off, by source name (`Steam`, `GOG`...).
    pub disabled_sources: Vec<String>,
}

mod amazon;
pub mod apps;
mod battlenet;
mod engines;
mod epic;
mod flashpoint;
pub mod folders;
mod gog;
pub mod installed;
mod itch;
mod linux_launchers;
pub mod lutris;
pub mod prefixes;
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
    /// The engines-database id of the engine a folder game was made with
    /// (`renpy`, `unity`...), when the folder says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

/// A program whose state the agent can read or sync with: launchers,
/// emulators, F95Checker, Playnite.
#[derive(Debug, Clone, Serialize)]
pub struct Program {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub note: String,
}

/// One source a scan reads (Steam, GOG, game folders...): whether the person
/// has it on, and how many games it found.
#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub name: String,
    pub enabled: bool,
    pub count: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Scan {
    pub sources: Vec<SourceReport>,
    pub games: Vec<Found>,
    /// The applications installed here, games aside (`installed`).
    pub apps: Vec<installed::InstalledApp>,
    /// Programs droidtop can sync with or learn from (`apps`).
    pub programs: Vec<Program>,
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

    /// Adds [`others`] (a desktop manager's own games) where this scan has no
    /// game with the same key or folder: the scanner's record of a game wins.
    pub fn join(&mut self, others: &[Found]) {
        let folder = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase();
        for other in others {
            let same_key = self.games.iter().any(|f| f.game.key == other.game.key);
            let same_folder = other.base.as_deref().is_some_and(|b| {
                let b = folder(b);
                self.games.iter().any(|f| f.base.as_deref().is_some_and(|fb| folder(fb) == b))
            });
            if !same_key && !same_folder {
                self.games.push(other.clone());
            }
        }
    }

    /// Everything the library records for this computer: its games and its
    /// installed applications.
    pub fn library(&self) -> Vec<ScannedGame> {
        self.games.iter().map(|g| g.game.clone()).chain(self.apps.iter().map(|a| a.to_library())).collect()
    }
}

/// One source of games: what it found, or why it could not read.
type Source = fn(&Options) -> Result<Vec<Found>, String>;

/// Scans this computer: every store and launcher it finds, and the folders
/// [`settings`] names. Slow (it walks the game folders); never call it from a
/// thread that draws.
pub fn scan(settings: &Options) -> Scan {
    let mut out = Scan::default();
    let sources: Vec<(&str, Source)> = vec![
        ("Steam", steam::scan),
        ("Epic", epic::scan),
        ("GOG", gog::scan),
        ("Amazon", amazon::scan),
        ("itch", itch::scan),
        ("Battle.net", battlenet::scan),
        ("Lutris", lutris::scan),
        ("Bottles and Minigalaxy", linux_launchers::scan),
        ("game folders", folders::scan),
        ("ROM folders", roms::scan),
    ];
    let mut by_key: BTreeMap<String, Found> = BTreeMap::new();
    for (name, source) in sources {
        let enabled = !settings.disabled_sources.iter().any(|d| d == name);
        out.sources.push(SourceReport { name: name.to_string(), enabled, count: 0 });
        if !enabled {
            continue;
        }
        match source(settings) {
            Ok(found) => {
                if let Some(report) = out.sources.last_mut() {
                    report.count = found.len();
                }
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
    // Heroic keeps a Wine or Proton prefix per game outside the game's
    // folder; elsewhere than Windows that is where its Windows saves are.
    if !cfg!(windows) {
        let heroic = prefixes::heroic_dirs();
        for f in out.games.iter_mut().filter(|f| f.prefix.is_none() && f.game.install.launcher.as_deref() == Some("heroic")) {
            if let Some((_, app)) = f.game.key.split_once(':') {
                f.prefix = prefixes::heroic(&heroic, app);
            }
        }
    }
    let game_folders: Vec<PathBuf> = out.games.iter().filter_map(|f| f.base.clone()).collect();
    out.apps = installed::scan(&game_folders);
    out.programs = apps::scan(settings);
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
        engine: None,
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
