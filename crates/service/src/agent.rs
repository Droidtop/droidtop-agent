//! The running agent: this computer's [`Store`], plus what only the running
//! agent keeps in memory: the last scan, the save catalog, the rendezvous's
//! state.

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
        let settings = self.store.settings.lock().unwrap().clone();
        let mut scan = droidtop_agent_scan::scan(&scan_options(&settings));
        scan.join(&ManagerGames::load(&self.store.dirs).usable(&self.store.dirs, &self.events));
        let launcher_marks = droidtop_agent_scan::lutris::marks();
        let (device, name) = (self.store.peer_id(), self.store.name());
        let changes = self.store.edit_library(|lib| {
            let n = lib.update_device(&device, &name, scan.library()) + self.store.note_launcher_marks(lib, launcher_marks);
            (n, n > 0)
        });
        *self.scan.lock().unwrap() = Some((Instant::now(), scan));
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
        let stale = match &*self.scan.lock().unwrap() {
            Some((at, _)) => at.elapsed().as_secs() > max_age_s,
            None => true,
        };
        if stale {
            self.rescan();
        }
        self.scan.lock().unwrap().as_ref().map(|(_, s)| s.clone()).unwrap_or_default()
    }
}
