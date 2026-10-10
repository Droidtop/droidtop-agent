//! The agent's own state on this computer: where it lives, its key, the
//! devices it is paired with, its settings, the library and the last scan.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::Library;
use serde::{Deserialize, Serialize};
use windowcast_identity::{Identity, TrustStore};

use crate::scan::Scan;

/// Where the agent keeps its files.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// Settings, the key and the paired devices.
    pub config: PathBuf,
    /// The library, the Ludusavi index and conflict archives.
    pub data: PathBuf,
}

impl Dirs {
    /// `DROIDTOP_AGENT_HOME` puts everything in one folder (portable use and
    /// tests); otherwise the platform's own places.
    pub fn locate() -> io::Result<Dirs> {
        let dirs = match std::env::var_os("DROIDTOP_AGENT_HOME") {
            Some(home) => Dirs { config: PathBuf::from(&home), data: PathBuf::from(home) },
            None => {
                let config =
                    dirs::config_dir().ok_or_else(|| io::Error::other("no settings folder on this system"))?.join("droidtop-agent");
                let data = dirs::data_local_dir().ok_or_else(|| io::Error::other("no data folder on this system"))?.join("droidtop-agent");
                Dirs { config, data }
            }
        };
        fs::create_dir_all(&dirs.config)?;
        fs::create_dir_all(&dirs.data)?;
        Ok(dirs)
    }

    pub fn identity(&self) -> PathBuf {
        self.config.join("identity.key")
    }
    pub fn trust(&self) -> PathBuf {
        self.config.join("trusted-devices")
    }
    pub fn peers(&self) -> PathBuf {
        self.config.join("devices.json")
    }
    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.json")
    }
    pub fn saves(&self) -> PathBuf {
        self.config.join("saves.json")
    }
    /// Adapters plugins offered that wait for the person's approval.
    pub fn adapter_offers(&self) -> PathBuf {
        self.data.join("adapter-offers.json")
    }
    pub fn adapters(&self) -> PathBuf {
        self.data.join("adapters")
    }
    /// The rendezvous's state, for `droidtop-agent rendezvous` to show.
    pub fn rendezvous(&self) -> PathBuf {
        self.data.join("rendezvous.json")
    }
    pub fn library(&self) -> PathBuf {
        self.data.join("library.json")
    }
    pub fn archive(&self) -> PathBuf {
        self.data.join("archive")
    }
}

fn scan_minutes() -> u64 {
    30
}

fn yes() -> bool {
    true
}

fn default_list() -> Vec<String> {
    vec!["default".into()]
}

/// The person's settings for this agent.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    /// This computer's name on the handheld; the host name when unset.
    #[serde(default)]
    pub name: Option<String>,
    /// Folders whose subfolders are games (`folders add`).
    #[serde(default)]
    pub game_folders: Vec<PathBuf>,
    /// Folders laid out by ES-DE system name (`folders add --roms`).
    #[serde(default)]
    pub rom_folders: Vec<PathBuf>,
    /// A folder the person's own sync tool carries to the handheld, for
    /// store and forward (`share set`).
    #[serde(default)]
    pub share: Option<PathBuf>,
    #[serde(default = "scan_minutes")]
    pub scan_minutes: u64,
    /// Where this computer's WireGuard port answers from the internet, when
    /// the person forwarded it on their router (`203.0.113.7:47611`).
    #[serde(default)]
    pub public_endpoint: Option<String>,
    /// Context adapters the person added (`contexts add`): the context each
    /// serves, and the program (docs/DESIGN.md section 8).
    #[serde(default)]
    pub adapters: BTreeMap<String, PathBuf>,
    /// The contexts whose adapter the person approved a plugin to supply
    /// (`contexts approve`): the plugin and the digest of the program in place.
    #[serde(default)]
    pub approved: BTreeMap<String, Approval>,
    /// Rendezvous away from the LAN through global discovery and STUN
    /// (`rendezvous on|off`; on unless the person turned it off).
    #[serde(default = "yes")]
    pub rendezvous: bool,
    /// Discovery servers in Syncthing's notation; `default` is Syncthing's.
    #[serde(default = "default_list")]
    pub discovery_servers: Vec<String>,
    /// STUN servers (`host:port`); `default` is the list Syncthing uses.
    #[serde(default = "default_list")]
    pub stun_servers: Vec<String>,
}

/// A fresh install's settings are the serde defaults (rendezvous on, Syncthing's servers).
impl Default for Settings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("every setting has a default")
    }
}

/// A plugin the person let supply a context's adapter, and what is installed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Approval {
    pub plugin: String,
    pub sha256: String,
}

/// A paired device as this computer remembers it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub paired_ms: i64,
    #[serde(default)]
    pub last_address: Option<String>,
    #[serde(default)]
    pub last_seen_ms: i64,
    /// Its global discovery ID, as it said in its last hello.
    #[serde(default)]
    pub disco: Option<String>,
}

/// Save locations the person states for a game, ahead of the Ludusavi manifest.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UserSaves {
    #[serde(default)]
    pub patterns: Vec<String>,
    /// The game's folder, when the scanner does not know it.
    #[serde(default)]
    pub base: Option<PathBuf>,
}

pub fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
    fs::rename(&tmp, path)
}

pub struct Agent {
    pub dirs: Dirs,
    pub key: DeviceKey,
    pub settings: Mutex<Settings>,
    pub trust: Mutex<TrustStore>,
    pub devices: Mutex<Vec<Device>>,
    pub saves: Mutex<BTreeMap<String, UserSaves>>,
    pub library: Mutex<Library>,
    pub scan: Mutex<Option<(Instant, Scan)>>,
    pub ludusavi: Mutex<Option<crate::ludusavi::Index>>,
    /// What the rendezvous knows now: the address the NAT gives the WireGuard
    /// socket, and where to punch (crate::rendezvous).
    pub rendezvous: std::sync::Arc<Mutex<crate::rendezvous::Shared>>,
}

impl Agent {
    pub fn open() -> io::Result<Agent> {
        let dirs = Dirs::locate()?;
        // windowcast's identity file, so the two can share one key on a
        // computer that runs both (docs/DESIGN.md section 3).
        let identity = Identity::load_or_generate(&dirs.identity()).map_err(|e| io::Error::other(e.to_string()))?;
        let key = DeviceKey::from_identity(&identity);
        let trust = TrustStore::load(&dirs.trust()).map_err(|e| io::Error::other(e.to_string()))?;
        let library = Library::load(&dirs.library())?;
        Ok(Agent {
            settings: Mutex::new(read_json(&dirs.settings())?),
            devices: Mutex::new(read_json(&dirs.peers())?),
            saves: Mutex::new(read_json(&dirs.saves())?),
            trust: Mutex::new(trust),
            library: Mutex::new(library),
            scan: Mutex::new(None),
            ludusavi: Mutex::new(None),
            rendezvous: Default::default(),
            key,
            dirs,
        })
    }

    pub fn name(&self) -> String {
        self.settings.lock().unwrap().name.clone().unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned())
    }

    pub fn peer_id(&self) -> PeerId {
        self.key.peer_id()
    }

    pub fn is_trusted(&self, peer: &PeerId) -> bool {
        self.trust.lock().unwrap().is_pinned(peer)
    }

    pub fn save_settings(&self) -> io::Result<()> {
        write_json(&self.dirs.settings(), &*self.settings.lock().unwrap())
    }

    pub fn save_saves(&self) -> io::Result<()> {
        write_json(&self.dirs.saves(), &*self.saves.lock().unwrap())
    }

    pub fn save_devices(&self) -> io::Result<()> {
        write_json(&self.dirs.peers(), &*self.devices.lock().unwrap())
    }

    pub fn save_library(&self) -> io::Result<()> {
        self.library.lock().unwrap().save(&self.dirs.library())
    }

    /// Pins a newly paired device and remembers its name.
    pub fn add_device(&self, peer: PeerId, name: &str) -> io::Result<()> {
        {
            let mut trust = self.trust.lock().unwrap();
            trust.pin(peer);
            trust.save(&self.dirs.trust()).map_err(|e| io::Error::other(e.to_string()))?;
        }
        {
            let mut devices = self.devices.lock().unwrap();
            devices.retain(|d| d.id != peer.to_hex());
            devices.push(Device {
                id: peer.to_hex(),
                name: name.to_string(),
                paired_ms: droidtop_agent_core::library::now_ms(),
                last_address: None,
                last_seen_ms: 0,
                disco: None,
            });
        }
        self.save_devices()
    }

    /// Forgets a device: unpinned, so it can no longer connect.
    pub fn remove_device(&self, peer: &PeerId) -> io::Result<()> {
        {
            let mut trust = self.trust.lock().unwrap();
            trust.revoke(peer);
            trust.save(&self.dirs.trust()).map_err(|e| io::Error::other(e.to_string()))?;
        }
        self.devices.lock().unwrap().retain(|d| d.id != peer.to_hex());
        self.save_devices()
    }

    /// Records the global discovery ID a paired device stated in its hello.
    pub fn device_disco(&self, peer: &PeerId, disco: &str) {
        if !droidtop_agent_core::rendezvous::valid_device_id(disco) {
            return;
        }
        let mut devices = self.devices.lock().unwrap();
        let Some(d) = devices.iter_mut().find(|d| d.id == peer.to_hex()) else { return };
        if d.disco.as_deref() == Some(disco) {
            return;
        }
        d.disco = Some(disco.to_string());
        drop(devices);
        let _ = self.save_devices();
    }

    pub fn seen(&self, peer: &PeerId, address: Option<String>) {
        let mut devices = self.devices.lock().unwrap();
        if let Some(d) = devices.iter_mut().find(|d| d.id == peer.to_hex()) {
            d.last_seen_ms = droidtop_agent_core::library::now_ms();
            if address.is_some() {
                d.last_address = address;
            }
        }
        drop(devices);
        let _ = self.save_devices();
    }

    /// Scans this computer, records what changed in the library, and keeps
    /// the scan for save lookups. Returns how many library changes it made.
    pub fn rescan(&self) -> usize {
        let settings = self.settings.lock().unwrap().clone();
        let scan = crate::scan::scan(&settings, &self.dirs);
        let changes = {
            let mut lib = self.library.lock().unwrap();
            lib.update_device(&self.peer_id(), &self.name(), scan.library())
        };
        if changes > 0 {
            let _ = self.save_library();
        }
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
