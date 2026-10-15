//! The running agent: this computer's [`Store`], plus what only the running
//! agent keeps in memory: the last scan, the save catalog, the rendezvous's
//! state and the games a desktop manager hands it.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use droidtop_agent_scan::{Found, Scan};
use droidtop_agent_store::{read_json, write_json, Dirs, Settings, Store};
use serde::{Deserialize, Serialize};

/// Save locations the person states for a game, ahead of the Ludusavi manifest.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UserSaves {
    #[serde(default)]
    pub patterns: Vec<String>,
    /// The game's folder, when the scanner does not know it.
    #[serde(default)]
    pub base: Option<PathBuf>,
}

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
    pub saves: Mutex<BTreeMap<String, UserSaves>>,
    pub scan: Mutex<Option<(Instant, Scan)>>,
    pub ludusavi: Mutex<Option<crate::ludusavi::Index>>,
    /// What the rendezvous knows now: the address the NAT gives the WireGuard
    /// socket, and where to punch (crate::rendezvous).
    pub rendezvous: std::sync::Arc<Mutex<crate::rendezvous::Shared>>,
    /// Games a desktop manager that runs this agent in its own process
    /// tracks itself (Pythia, through `crates/python`): joined into every
    /// scan where the scanner has not found them (docs/DESIGN.md section 16).
    pub extra: Mutex<Vec<Found>>,
}

impl Agent {
    pub fn open() -> io::Result<Agent> {
        Agent::open_in(Dirs::locate()?)
    }

    /// The agent over [`dirs`] (`open` is this over the system's places).
    pub fn open_in(dirs: Dirs) -> io::Result<Agent> {
        let store = Store::open_in(dirs)?;
        Ok(Agent {
            saves: Mutex::new(read_json(&store.dirs.saves())?),
            scan: Mutex::new(None),
            ludusavi: Mutex::new(None),
            rendezvous: Default::default(),
            extra: Mutex::new(Vec::new()),
            store,
        })
    }

    pub fn save_saves(&self) -> io::Result<()> {
        write_json(&self.store.dirs.saves(), &*self.saves.lock().unwrap())
    }

    /// Scans this computer, records what changed in the library, and keeps
    /// the scan for save lookups. Returns how many library changes it made.
    /// The scan only ever writes this computer's own installs: marks the
    /// person set (here or on a handheld) are not touched by it.
    pub fn rescan(&self) -> usize {
        self.store.reload_settings();
        let settings = self.store.settings.lock().unwrap().clone();
        let mut scan = droidtop_agent_scan::scan(&scan_options(&settings));
        scan.join(&self.extra.lock().unwrap());
        let launcher_marks = droidtop_agent_scan::lutris::marks();
        let (device, name) = (self.store.peer_id(), self.store.name());
        let changes = self.store.edit_library(|lib| {
            let n = lib.update_device(&device, &name, scan.library()) + self.store.note_launcher_marks(lib, launcher_marks);
            (n, n > 0)
        });
        *self.scan.lock().unwrap() = Some((Instant::now(), scan));
        changes
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
