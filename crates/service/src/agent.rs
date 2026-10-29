//! The running agent: this computer's [`Store`], plus what only the running
//! agent keeps in memory: the last scan, the save catalog, the rendezvous's
//! state.

use droidtop_agent_core::sync::LockExt;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_saves::Saves;
use droidtop_agent_scan::{Found, Scan};
use droidtop_agent_store::{Dirs, Settings, Store};

use crate::events::Events;
use crate::manager::{ManagerGame, ManagerGames};

/// What the scanner looks at, from the person's settings.
pub fn scan_options(settings: &Settings) -> droidtop_agent_scan::Options {
    droidtop_agent_scan::Options {
        game_folders: settings.game_folders.clone(),
        rom_folders: settings.rom_folders.clone(),
        disabled_folders: settings.disabled_folders.clone(),
        disabled_sources: settings.disabled_sources.clone(),
    }
}

pub struct Agent {
    /// The state on disk, shared with every other copy of the agent.
    pub store: Store,
    /// Where games keep their saves here: the person's entries and the Ludusavi index.
    pub saves: Saves,
    pub scan: Mutex<Option<(Instant, Scan)>>,
    /// What the rendezvous knows now: the address the NAT gives the WireGuard
    /// socket, and where to punch (crate::rendezvous).
    pub rendezvous: std::sync::Arc<Mutex<crate::rendezvous::Shared>>,
    /// What the running agent did, for the program that hosts it.
    pub events: Events,
    /// The program hosting the agent, when it starts games of its own
    /// (crate::launch::Launcher): Pythia, for the games it tracks.
    pub launcher: Mutex<Option<std::sync::Arc<dyn crate::launch::Launcher>>>,
}

impl Agent {
    pub fn open() -> io::Result<Agent> {
        Agent::open_in(Dirs::locate()?)
    }

    /// The agent over [`dirs`] (`open` is this over the system's places).
    pub fn open_in(dirs: Dirs) -> io::Result<Agent> {
        let store = Store::open_in(dirs)?;
        Ok(Agent {
            saves: Saves::open(store.dirs.saves(), store.dirs.data.clone())?,
            scan: Mutex::new(None),
            rendezvous: Default::default(),
            events: Events::default(),
            launcher: Mutex::new(None),
            store,
        })
    }

    /// Where a game keeps its saves on this computer ([`Saves::lookup`]),
    /// looked up in a scan at most 5 minutes old: this may rescan, which is slow.
    pub fn save_lookup(&self, game: &GameRef, fetch: bool) -> Option<(SaveSpec, Roots, Option<Found>)> {
        self.saves.lookup(game, &self.fresh_scan(300), fetch)
    }

    /// What this agent syncs of a game's saves, and what it leaves to the
    /// Syncthing already running here (Droidtop/tracker#538). A save location
    /// inside a folder Syncthing syncs is Syncthing's: two programs writing the
    /// same files would fight. An engine that keeps one set of saves in several
    /// places and merges them itself (Ren'Py) is Syncthing's as a whole once
    /// Syncthing carries any one of them. None when no save location is known.
    pub fn save_plan(&self, game: &GameRef, fetch: bool) -> Option<SavePlan> {
        let (spec, roots, found) = self.save_lookup(game, fetch)?;
        let folders = crate::syncthing::home().map(|h| crate::syncthing::folders(&h)).unwrap_or_default();
        Some(SavePlan::new(spec, roots, found, &folders))
    }

    /// Puts a kept copy of a game's saves back ([`Saves::restore`]); what is
    /// there now is kept first as this computer's own copy.
    pub fn restore_saves(&self, game: &GameRef, copy: &Path) -> Result<usize, String> {
        self.saves.restore(game, copy, &self.fresh_scan(300), &self.store.dirs.archive(), &self.store.name())
    }

    /// Scans this computer, records what changed in the library, and keeps
    /// the scan for save lookups. Returns how many library changes it made.
    /// The scan only ever writes this computer's own installs: marks the
    /// person set (here or on a handheld) are not touched by it.
    pub fn rescan(&self) -> usize {
        self.store.reload_settings();
        let settings = self.store.settings.locked().clone();
        let mut scan = droidtop_agent_scan::scan(&scan_options(&settings));
        scan.join(&ManagerGames::load(&self.store.dirs).usable(&self.store.dirs, &self.events));
        let launcher_marks = droidtop_agent_scan::lutris::marks();
        let (device, name) = (self.store.peer_id(), self.store.name());
        let changes = self.store.edit_library(|lib| {
            let n = lib.update_device(&device, &name, scan.library()) + self.store.note_launcher_marks(lib, launcher_marks);
            (n, n > 0)
        });
        *self.scan.locked() = Some((Instant::now(), scan));
        changes
    }

    /// The games desktop manager [`manager`] (Pythia) tracks itself, in
    /// place of the ones it handed over before: kept in the agent's data
    /// folder and joined into every rescan from now on, whichever program
    /// serves (docs/DESIGN.md section 16). An empty list clears them.
    pub fn set_manager_games(&self, manager: &str, games: Vec<ManagerGame>) -> io::Result<()> {
        ManagerGames { manager: manager.to_string(), games }.save(&self.store.dirs)
    }

    /// The last scan, made now when there is none or it is older than [`max_age_s`].
    pub fn fresh_scan(&self, max_age_s: u64) -> Scan {
        let stale = match &*self.scan.locked() {
            Some((at, _)) => at.elapsed().as_secs() > max_age_s,
            None => true,
        };
        if stale {
            self.rescan();
        }
        self.scan.locked().as_ref().map(|(_, s)| s.clone()).unwrap_or_default()
    }
}

/// A game's saves as this agent syncs them ([`Agent::save_plan`]).
#[derive(Debug, Clone)]
pub struct SavePlan {
    /// Every location known for the game's saves.
    pub all: SaveSpec,
    /// The ones this agent syncs: [`all`] less Syncthing's.
    pub spec: SaveSpec,
    pub roots: Roots,
    pub found: Option<Found>,
    /// Each location left to Syncthing, with the label of its folder there.
    pub by_syncthing: Vec<(String, crate::syncthing::Folder)>,
}

impl SavePlan {
    pub fn new(all: SaveSpec, roots: Roots, found: Option<Found>, folders: &[crate::syncthing::Folder]) -> SavePlan {
        let mut by_syncthing = Vec::new();
        let mut kept = Vec::new();
        for pattern in &all.patterns {
            match fixed_folder(pattern, &roots).and_then(|path| crate::syncthing::holding(folders, &path)) {
                Some(folder) => by_syncthing.push((pattern.clone(), folder.clone())),
                None => kept.push(pattern.clone()),
            }
        }
        let mirrors = droidtop_agent_saves::engines::mirrors(found.as_ref().and_then(|f| f.engine.as_deref()));
        if mirrors && !by_syncthing.is_empty() {
            kept.clear();
        }
        SavePlan { spec: SaveSpec { patterns: kept }, all, roots, found, by_syncthing }
    }

    /// Whether Syncthing carries all of the game's saves, so this agent syncs none.
    pub fn all_by_syncthing(&self) -> bool {
        self.spec.patterns.is_empty() && !self.by_syncthing.is_empty()
    }
}

/// The folder a save template names before its first wildcard, on this
/// computer: what Syncthing would have to hold for the template's files to be its.
fn fixed_folder(pattern: &str, roots: &Roots) -> Option<std::path::PathBuf> {
    let (token, parts) = droidtop_agent_core::saves::split(pattern)?;
    let mut path = roots.get(token)?.clone();
    for part in parts.iter().take_while(|p| !p.contains(['*', '?'])) {
        path.push(part);
    }
    Some(path)
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::syncthing::Folder;
    use std::path::PathBuf;

    fn found(engine: Option<&str>) -> Option<Found> {
        Some(Found {
            game: droidtop_agent_core::library::ScannedGame {
                key: "title:g".into(),
                title: "G".into(),
                platform: None,
                install: Default::default(),
            },
            base: Some(PathBuf::from("/sync/GameSync/G")),
            prefix: None,
            engine: engine.map(str::to_string),
        })
    }

    fn roots() -> Roots {
        Roots::from([("<base>".to_string(), PathBuf::from("/sync/GameSync/G")), ("<home>".to_string(), PathBuf::from("/home/p"))])
    }

    #[test]
    fn a_location_inside_a_syncthing_folder_is_left_to_it() {
        let folders = [Folder { label: "GameSync".into(), path: PathBuf::from("/sync/GameSync") }];
        let all = SaveSpec { patterns: vec!["<base>/saves/*.sav".into(), "<home>/.local/share/G".into()] };
        let plan = SavePlan::new(all, roots(), found(None), &folders);
        assert_eq!(plan.spec.patterns, ["<home>/.local/share/G"]);
        assert_eq!(plan.by_syncthing.len(), 1);
        assert_eq!(plan.by_syncthing[0].1.label, "GameSync");
        assert!(!plan.all_by_syncthing());
    }

    #[test]
    fn renpy_saves_are_syncthings_as_a_whole_once_it_carries_one_copy() {
        let folders = [Folder { label: "GameSync".into(), path: PathBuf::from("/sync/GameSync") }];
        let all = SaveSpec { patterns: vec!["<base>/game/saves".into(), "<home>/.renpy/G-1".into()] };
        let plan = SavePlan::new(all.clone(), roots(), found(Some("renpy")), &folders);
        assert!(plan.all_by_syncthing());
        // Without Syncthing, every location is the agent's.
        let plan = SavePlan::new(all.clone(), roots(), found(Some("renpy")), &[]);
        assert_eq!(plan.spec, all);
        assert!(plan.by_syncthing.is_empty());
    }

    #[test]
    fn a_folder_merely_named_alike_is_not_inside() {
        let folders = [Folder { label: "Game".into(), path: PathBuf::from("/sync/Game") }];
        let all = SaveSpec { patterns: vec!["<base>/saves".into()] };
        let plan = SavePlan::new(all.clone(), roots(), found(None), &folders);
        assert_eq!(plan.spec, all);
    }
}
