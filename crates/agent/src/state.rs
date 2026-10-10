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
    /// Settings and the paired devices' names.
    pub config: PathBuf,
    /// The library, the Ludusavi index and the save archives.
    pub data: PathBuf,
    /// This computer's identity and trusted devices: windowcast's host folder
    /// (`windowcast_identity::computer_dir`), shared with windowcast, or the
    /// agent's own folder when `DROIDTOP_AGENT_HOME` makes it portable.
    pub computer: PathBuf,
}

impl Dirs {
    /// `DROIDTOP_AGENT_HOME` puts everything in one folder (portable use and
    /// tests); otherwise the platform's own places.
    pub fn locate() -> io::Result<Dirs> {
        let dirs = match std::env::var_os("DROIDTOP_AGENT_HOME") {
            Some(home) => Dirs { config: PathBuf::from(&home), data: PathBuf::from(&home), computer: PathBuf::from(home) },
            None => {
                let config =
                    dirs::config_dir().ok_or_else(|| io::Error::other("no settings folder on this system"))?.join("droidtop-agent");
                let data = dirs::data_local_dir().ok_or_else(|| io::Error::other("no data folder on this system"))?.join("droidtop-agent");
                Dirs { config, data, computer: windowcast_identity::computer_dir() }
            }
        };
        fs::create_dir_all(&dirs.config)?;
        fs::create_dir_all(&dirs.data)?;
        fs::create_dir_all(&dirs.computer)?;
        Ok(dirs)
    }

    /// This computer's identity, shared with windowcast's host.
    pub fn identity(&self) -> PathBuf {
        self.computer.join(windowcast_identity::HOST_IDENTITY_FILE)
    }
    /// The devices this computer trusts, shared with windowcast's host.
    pub fn trust(&self) -> PathBuf {
        self.computer.join(windowcast_identity::HOST_TRUST_FILE)
    }
    /// Where the agent kept its own identity and trusted devices before it
    /// shared windowcast's.
    pub fn own_identity(&self) -> PathBuf {
        self.config.join("identity.key")
    }
    pub fn own_trust(&self) -> PathBuf {
        self.config.join("trusted-devices")
    }
    /// The identity this computer is moving away from, kept until every
    /// paired handheld has heard of the move (crate::state::migrate).
    pub fn previous_identity(&self) -> PathBuf {
        self.config.join("previous-identity.key")
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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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
    /// The contexts whose offered adapter the person declined, and the plugin.
    #[serde(default)]
    pub declined: BTreeMap<String, String>,
    /// The handheld whose saves this computer prefers when both sides
    /// changed (`primary`), by id; otherwise the newest copy wins.
    #[serde(default)]
    pub primary_device: Option<String>,
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
    /// It has reached this computer at its current identity since the
    /// computer moved to it (crate::state::migrate).
    #[serde(default)]
    pub moved: bool,
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

/// Moves the agent onto the identity and trusted list it shares with
/// windowcast's host (docs/DESIGN.md section 3, "One identity with
/// windowcast"), once. [`paired`]: the agent has paired handhelds.
/// - Only the agent had an identity: it becomes the shared one; nothing
///   changes for the paired handhelds.
/// - Both had one: the shared (windowcast's) identity stays. With paired
///   handhelds the agent keeps its own as the previous identity, answers to
///   both, and tells each handheld of the move ([`Agent::moved`]).
/// - The agent's trusted handhelds join the shared list.
///
/// The agent's own files are renamed `*.moved`, so this runs once.
pub fn migrate(dirs: &Dirs, paired: bool) -> io::Result<()> {
    let (own_id, own_trust) = (dirs.own_identity(), dirs.own_trust());
    if own_id == dirs.identity() || !own_id.is_file() {
        return Ok(());
    }
    let own = fs::read(&own_id)?;
    match fs::read(dirs.identity()) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::write(dirs.identity(), &own)?,
        Err(e) => return Err(e),
        Ok(shared) if shared != own && paired => fs::write(dirs.previous_identity(), &own)?,
        Ok(_) => {}
    }
    if own_trust.is_file() {
        let theirs = TrustStore::load(&own_trust).map_err(|e| io::Error::other(e.to_string()))?;
        TrustStore::update(&dirs.trust(), |t| theirs.peers().for_each(|p| t.pin(*p))).map_err(|e| io::Error::other(e.to_string()))?;
        fs::rename(&own_trust, own_trust.with_extension("moved"))?;
    }
    fs::rename(&own_id, own_id.with_extension("key.moved"))?;
    Ok(())
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
    /// The identity this computer is moving away from (crate::state::migrate),
    /// still answered until every paired handheld has heard of the move.
    pub previous: Option<DeviceKey>,
    pub settings: Mutex<Settings>,
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
        let devices: Vec<Device> = read_json(&dirs.peers())?;
        migrate(&dirs, !devices.is_empty())?;
        // windowcast's identity file, so a computer running both has one key
        // and one pairing (docs/DESIGN.md section 3).
        let identity = Identity::load_or_generate(&dirs.identity()).map_err(|e| io::Error::other(e.to_string()))?;
        let key = DeviceKey::from_identity(&identity);
        let previous = fs::read(dirs.previous_identity()).ok().and_then(|seed| DeviceKey::from_seed(&seed).ok());
        let library = Library::load(&dirs.library())?;
        Ok(Agent {
            settings: Mutex::new(read_json(&dirs.settings())?),
            devices: Mutex::new(devices),
            saves: Mutex::new(read_json(&dirs.saves())?),
            previous,
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

    /// Whether [`peer`] is the handheld whose saves this computer prefers.
    pub fn is_primary(&self, peer: &PeerId) -> bool {
        self.settings.lock().unwrap().primary_device.as_deref() == Some(peer.to_hex().as_str())
    }

    /// A paired device's name, or the start of its id.
    pub fn device_name(&self, peer: &PeerId) -> String {
        let id = peer.to_hex();
        self.devices.lock().unwrap().iter().find(|d| d.id == id).map(|d| d.name.clone()).unwrap_or_else(|| id[..16].to_string())
    }

    /// Whether [`peer`] is paired: the trusted list on disk, which
    /// windowcast's host changes too.
    pub fn is_trusted(&self, peer: &PeerId) -> bool {
        TrustStore::load(&self.dirs.trust()).is_ok_and(|t| t.is_pinned(peer))
    }

    /// The move this computer tells a handheld that reached it at its
    /// previous identity.
    pub fn moved(&self) -> Option<droidtop_agent_core::moved::Moved> {
        self.previous.as_ref().map(|old| droidtop_agent_core::moved::Moved::new(old, &self.key))
    }

    /// A paired handheld reached this computer at its current identity: once
    /// all have, the previous one is no longer needed and is deleted.
    pub fn on_current_key(&self, peer: &PeerId) {
        if self.previous.is_none() {
            return;
        }
        let all = {
            let mut devices = self.devices.lock().unwrap();
            if let Some(d) = devices.iter_mut().find(|d| d.id == peer.to_hex()) {
                d.moved = true;
            }
            devices.iter().all(|d| d.moved)
        };
        let _ = self.save_devices();
        if all {
            let _ = fs::remove_file(self.dirs.previous_identity());
        }
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
        TrustStore::update(&self.dirs.trust(), |t| t.pin(peer)).map_err(|e| io::Error::other(e.to_string()))?;
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
                moved: true,
            });
        }
        self.save_devices()
    }

    /// Forgets a device: unpinned, so it can no longer connect.
    pub fn remove_device(&self, peer: &PeerId) -> io::Result<()> {
        TrustStore::update(&self.dirs.trust(), |t| t.revoke(peer)).map_err(|e| io::Error::other(e.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(name: &str) -> Dirs {
        let root = std::env::temp_dir().join(format!("dta-migrate-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let d = Dirs { config: root.join("agent"), data: root.join("data"), computer: root.join("windowcast/app/host") };
        for p in [&d.config, &d.data, &d.computer] {
            fs::create_dir_all(p).unwrap();
        }
        d
    }

    fn own(d: &Dirs, trusted: &PeerId) -> Vec<u8> {
        let seed = DeviceKey::generate().seed().to_vec();
        fs::write(d.own_identity(), &seed).unwrap();
        TrustStore::update(&d.own_trust(), |t| t.pin(*trusted)).unwrap();
        seed
    }

    #[test]
    fn the_agents_identity_becomes_the_shared_one_when_windowcast_has_none() {
        let d = dirs("one");
        let handheld = DeviceKey::generate().peer_id();
        let seed = own(&d, &handheld);
        migrate(&d, true).unwrap();
        assert_eq!(fs::read(d.identity()).unwrap(), seed);
        assert!(TrustStore::load(&d.trust()).unwrap().is_pinned(&handheld));
        assert!(!d.previous_identity().exists() && !d.own_identity().exists());
        // It runs once.
        migrate(&d, true).unwrap();
        assert_eq!(fs::read(d.identity()).unwrap(), seed);
    }

    #[test]
    fn with_both_identities_and_pairings_the_agent_moves_to_windowcasts() {
        let d = dirs("both");
        let (handheld, viewer) = (DeviceKey::generate().peer_id(), DeviceKey::generate().peer_id());
        let shared = DeviceKey::generate().seed().to_vec();
        fs::write(d.identity(), &shared).unwrap();
        TrustStore::update(&d.trust(), |t| t.pin(viewer)).unwrap();
        let seed = own(&d, &handheld);
        migrate(&d, true).unwrap();
        assert_eq!(fs::read(d.identity()).unwrap(), shared);
        assert_eq!(fs::read(d.previous_identity()).unwrap(), seed);
        let trust = TrustStore::load(&d.trust()).unwrap();
        assert!(trust.is_pinned(&handheld) && trust.is_pinned(&viewer));
    }

    #[test]
    fn without_pairings_the_agent_simply_adopts_windowcasts() {
        let d = dirs("adopt");
        let shared = DeviceKey::generate().seed().to_vec();
        fs::write(d.identity(), &shared).unwrap();
        fs::write(d.own_identity(), DeviceKey::generate().seed()).unwrap();
        migrate(&d, false).unwrap();
        assert_eq!(fs::read(d.identity()).unwrap(), shared);
        assert!(!d.previous_identity().exists());
    }
}
