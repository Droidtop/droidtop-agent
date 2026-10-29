//! This computer's droidtop agent state on disk: where it lives, its
//! identity (shared with windowcast's host), the devices it is paired with,
//! the person's settings and the library with its change log. The running
//! agent (droidtop-agent-service), the window, the command line and a
//! desktop manager that is this computer's agent (Pythia) each open a
//! [`Store`] over the same files, so either program can take over from the
//! other with nothing to move (docs/DESIGN.md sections 3, 13 and 16).
//!
//! It does no networking and knows nothing of scanning or saves.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::Library;
use droidtop_agent_core::pairing::Kind;
use droidtop_agent_core::proto::{check_announcements, Announcements, Offer};
use serde::{Deserialize, Serialize};
use windowcast_identity::{Identity, TrustStore};

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
    /// paired handheld has heard of the move ([`migrate`]).
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
    /// The games a desktop manager (Pythia) tracks itself.
    pub fn manager_games(&self) -> PathBuf {
        self.data.join("manager-games.json")
    }
    pub fn archive(&self) -> PathBuf {
        self.data.join("archive")
    }
    /// The Syncthing devices this agent added for paired devices, so it
    /// removes only those (docs/DESIGN.md section 18).
    pub fn syncthing(&self) -> PathBuf {
        self.data.join("syncthing.json")
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
    /// Game folders the person switched off in the Library page: they stay
    /// listed (a folder found on its own, or one they added) but a scan skips them.
    #[serde(default)]
    pub disabled_folders: Vec<PathBuf>,
    /// Launchers and stores the person switched off (`steam`, `gog`...).
    #[serde(default)]
    pub disabled_sources: Vec<String>,
    /// The handhelds (by id) the person let start games on this computer
    /// (`launch allow`); none until they do (docs/DESIGN.md section 17).
    #[serde(default)]
    pub launch_devices: Vec<String>,
    /// What this computer offers each paired device, by id, where the person
    /// set it (docs/DESIGN.md section 20). A device not listed gets its
    /// kind's default ([`Store::offers_to`]).
    #[serde(default)]
    pub offers: BTreeMap<String, Offer>,
    /// What this computer announces to its paired devices, per plugin
    /// (`set_announce`; Droidtop/tracker#556): a desktop manager's Syncthing
    /// module puts its device ID here.
    #[serde(default)]
    pub announce: Announcements,
    /// Keep this computer's Syncthing in step with the paired devices'
    /// (docs/DESIGN.md section 18): on unless the person turned it off.
    #[serde(default = "yes")]
    pub syncthing: bool,
}

/// A fresh install's settings are the serde defaults (rendezvous on, Syncthing's servers).
impl Default for Settings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("every setting has a default")
    }
}

/// What this computer offers paired [`device`] under [`settings`]: what the
/// person set, or else its kind's default (docs/DESIGN.md section 20).
pub fn offer_for(settings: &Settings, device: &Device) -> Offer {
    if let Some(set) = settings.offers.get(&device.id) {
        return *set;
    }
    match device.kind {
        Some(Kind::Computer) => Offer::NONE,
        // A device paired before pairing said its kind is a handheld.
        Some(Kind::Handheld) | None => Offer::LIBRARY_AND_SAVES,
    }
}

/// A plugin the person let supply a context's adapter, and what is installed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Approval {
    pub plugin: String,
    pub sha256: String,
}

/// A paired device as this computer remembers it.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
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
    /// computer moved to it ([`migrate`]).
    #[serde(default)]
    pub moved: bool,
    /// A handheld or a computer, as it said when pairing; none for one
    /// paired before pairing said (a handheld).
    #[serde(default)]
    pub kind: Option<Kind>,
    /// What it announced last, per plugin, and when.
    #[serde(default)]
    pub announced: Announcements,
    #[serde(default)]
    pub announced_ms: i64,
}

/// Moves the agent onto the identity and trusted list it shares with
/// windowcast's host (docs/DESIGN.md section 3, "One identity with
/// windowcast"), once. [`paired`]: the agent has paired handhelds.
/// - Only the agent had an identity: it becomes the shared one; nothing
///   changes for the paired handhelds.
/// - Both had one: the shared (windowcast's) identity stays. With paired
///   handhelds the agent keeps its own as the previous identity, answers to
///   both, and tells each handheld of the move ([`Store::moved`]).
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

/// What a file looked like when it was read: modified time and length.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    fs::metadata(path).ok().and_then(|m| Some((m.modified().ok()?, m.len())))
}

/// An operating system lock on a file beside a state file, so the headless
/// agent, the window and a command line run at the same time change it one
/// after another and never write back a copy that is out of date
/// (Droidtop/tracker#528, #531). The system holds it: it is released when
/// this is dropped and when the process dies, so a holder that is slow is
/// waited for and one that died is not (the lock file used to be removed after
/// ten seconds whoever held it, which let a second writer in beside a slow
/// first one). The file itself stays, since removing it while others wait
/// would let two of them hold "the" lock on different files.
struct FileLock(fs::File);

impl FileLock {
    /// Waits for the lock on [`of`]'s lock file. An error means the system would not lock the file (a read-only folder, a
    /// file system without locks): the caller goes on without, as a write to a folder that refuses locks fails the same way.
    fn take(of: &Path) -> io::Result<FileLock> {
        let file = fs::OpenOptions::new().write(true).create(true).truncate(false).open(of.with_extension("lock"))?;
        file.lock()?;
        Ok(FileLock(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Unlocking can only fail for a file already closed; closing the file releases the lock in any case.
        let _ = self.0.unlock();
    }
}

/// The agent's state on this computer, as one process sees it. Copies in
/// other processes (the headless agent, the window, a command line, Pythia)
/// see each other's changes: the device list and the library are read again
/// when their file changed, and changed under a lock file.
pub struct Store {
    pub dirs: Dirs,
    pub key: DeviceKey,
    /// The identity this computer is moving away from ([`migrate`]),
    /// still answered until every paired handheld has heard of the move.
    pub previous: Option<DeviceKey>,
    pub settings: Mutex<Settings>,
    /// The paired devices as last read; [`Store::devices_now`] and
    /// [`Store::edit_devices`] keep it current with the file.
    devices: Mutex<Vec<Device>>,
    devices_stamp: Mutex<Stamp>,
    /// The library as last read; [`Store::library_now`] and
    /// [`Store::edit_library`] keep it current with the file.
    library: Mutex<Library>,
    library_stamp: Mutex<Stamp>,
}

impl Store {
    pub fn open() -> io::Result<Store> {
        Store::open_in(Dirs::locate()?)
    }

    /// The agent over [`dirs`] (`open` is this over the system's places).
    pub fn open_in(dirs: Dirs) -> io::Result<Store> {
        let devices: Vec<Device> = read_json(&dirs.peers())?;
        migrate(&dirs, !devices.is_empty())?;
        // windowcast's identity file, so a computer running both has one key
        // and one pairing (docs/DESIGN.md section 3).
        let identity = Identity::load_or_generate(&dirs.identity()).map_err(|e| io::Error::other(e.to_string()))?;
        let key = DeviceKey::from_identity(&identity);
        let previous = fs::read(dirs.previous_identity()).ok().and_then(|seed| DeviceKey::from_seed(&seed).ok());
        let library = Library::load(&dirs.library())?;
        Ok(Store {
            settings: Mutex::new(read_json(&dirs.settings())?),
            devices_stamp: Mutex::new(stamp(&dirs.peers())),
            devices: Mutex::new(devices),
            previous,
            library_stamp: Mutex::new(stamp(&dirs.library())),
            library: Mutex::new(library),
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

    /// Whether the person let [`peer`] start games on this computer.
    pub fn may_launch(&self, peer: &PeerId) -> bool {
        self.settings.lock().unwrap().launch_devices.contains(&peer.to_hex())
    }

    /// What this computer offers [`peer`]: what the person set, or else a
    /// handheld's default, its library and its saves (a handheld assumes the
    /// computer it pairs with is its library and save host). A computer, and
    /// a device that is not paired, are offered nothing.
    pub fn offers_to(&self, peer: &PeerId) -> Offer {
        let id = peer.to_hex();
        match self.devices_now().iter().find(|d| d.id == id) {
            Some(device) => offer_for(&self.settings.lock().unwrap(), device),
            None => Offer::NONE,
        }
    }

    /// Sets what this computer offers the paired device [`id`]. Saves need
    /// the library: offering saves offers the library too, and withdrawing
    /// the library withdraws the saves. Call [`Store::save_settings`] after.
    pub fn set_offer(&self, id: &str, library: Option<bool>, saves: Option<bool>) {
        let peer = PeerId::from_hex(id);
        let mut offer = peer.map(|p| self.offers_to(&p)).unwrap_or_default();
        if let Some(on) = library {
            offer.library = on;
            offer.saves &= on;
        }
        if let Some(on) = saves {
            offer.saves = on;
            offer.library |= on;
        }
        self.settings.lock().unwrap().offers.insert(id.to_string(), offer);
    }

    /// Whether [`peer`] paired as a computer.
    pub fn is_computer(&self, peer: &PeerId) -> bool {
        let id = peer.to_hex();
        self.devices_now().iter().any(|d| d.id == id && d.kind == Some(Kind::Computer))
    }

    /// Whether [`peer`] may sync plugin contexts here (section 8): a paired
    /// handheld, never a computer.
    pub fn syncs_contexts(&self, peer: &PeerId) -> bool {
        let id = peer.to_hex();
        matches!(self.devices_now().iter().find(|d| d.id == id).map(|d| d.kind), Some(Some(Kind::Handheld) | None))
    }

    /// A paired device's name, or the start of its id.
    pub fn device_name(&self, peer: &PeerId) -> String {
        let id = peer.to_hex();
        self.devices_now().iter().find(|d| d.id == id).map(|d| d.name.clone()).unwrap_or_else(|| id[..16].to_string())
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
        let all = self.edit_devices(|devices| {
            let found = devices.iter_mut().find(|d| d.id == peer.to_hex());
            if let Some(d) = found {
                d.moved = true;
            }
            (devices.iter().all(|d| d.moved), true)
        });
        if all {
            let _ = fs::remove_file(self.dirs.previous_identity());
        }
    }

    pub fn save_settings(&self) -> io::Result<()> {
        write_json(&self.dirs.settings(), &*self.settings.lock().unwrap())
    }

    /// Takes in settings another copy of the agent (the window, a command
    /// line) saved since this one read them.
    pub fn reload_settings(&self) {
        if let Ok(fresh) = read_json::<Settings>(&self.dirs.settings()) {
            *self.settings.lock().unwrap() = fresh;
        }
    }

    /// The paired devices, as the file holds them now: the headless agent,
    /// the window and a command line each have their own copy of this
    /// state, and a pairing made by one is seen by the others.
    pub fn devices_now(&self) -> Vec<Device> {
        let mut devices = self.devices.lock().unwrap();
        let mut seen = self.devices_stamp.lock().unwrap();
        let on_disk = stamp(&self.dirs.peers());
        if on_disk != *seen {
            if let Ok(fresh) = read_json::<Vec<Device>>(&self.dirs.peers()) {
                *devices = fresh;
            }
            *seen = on_disk;
        }
        devices.clone()
    }

    /// Changes the paired devices from the file's current state and writes
    /// them back; [`f`] returns its result and whether it changed anything.
    pub fn edit_devices<R>(&self, f: impl FnOnce(&mut Vec<Device>) -> (R, bool)) -> R {
        let _lock = FileLock::take(&self.dirs.peers()).ok();
        self.devices_now();
        let mut devices = self.devices.lock().unwrap();
        let (result, changed) = f(&mut devices);
        if changed && write_json(&self.dirs.peers(), &*devices).is_ok() {
            *self.devices_stamp.lock().unwrap() = stamp(&self.dirs.peers());
        }
        result
    }

    /// The library, as the file holds it now (see [`Store::devices_now`]).
    /// Do not hold it across a call that edits the library.
    pub fn library_now(&self) -> MutexGuard<'_, Library> {
        let mut library = self.library.lock().unwrap();
        let mut seen = self.library_stamp.lock().unwrap();
        let on_disk = stamp(&self.dirs.library());
        if on_disk != *seen {
            if let Ok(fresh) = Library::load(&self.dirs.library()) {
                *library = fresh;
            }
            *seen = on_disk;
        }
        library
    }

    /// Changes the library from the file's current state and writes it back,
    /// one process at a time; [`f`] returns its result and whether it changed
    /// anything. This is the only way the agent writes `library.json`, so a
    /// copy that is out of date is never written over newer marks or installs.
    pub fn edit_library<R>(&self, f: impl FnOnce(&mut Library) -> (R, bool)) -> R {
        let _lock = FileLock::take(&self.dirs.library()).ok();
        let mut library = self.library_now();
        let (result, changed) = f(&mut library);
        if changed && library.save(&self.dirs.library()).is_ok() {
            *self.library_stamp.lock().unwrap() = stamp(&self.dirs.library());
        }
        result
    }

    /// Pins a newly paired device and remembers its name and kind.
    pub fn add_device(&self, peer: PeerId, name: &str, kind: Kind) -> io::Result<()> {
        TrustStore::update(&self.dirs.trust(), |t| t.pin(peer)).map_err(|e| io::Error::other(e.to_string()))?;
        self.edit_devices(|devices| {
            devices.retain(|d| d.id != peer.to_hex());
            devices.push(Device {
                id: peer.to_hex(),
                name: name.to_string(),
                paired_ms: droidtop_agent_core::library::now_ms(),
                last_address: None,
                last_seen_ms: 0,
                disco: None,
                moved: true,
                kind: Some(kind),
                announced: Announcements::new(),
                announced_ms: 0,
            });
            ((), true)
        });
        Ok(())
    }

    /// Keeps what paired device [`peer`] announced, in place of what it
    /// announced before. Returns whether anything changed.
    pub fn device_announced(&self, peer: &PeerId, values: Announcements) -> bool {
        self.edit_devices(|devices| match devices.iter_mut().find(|d| d.id == peer.to_hex()) {
            Some(d) if d.announced != values => {
                d.announced = values;
                d.announced_ms = droidtop_agent_core::library::now_ms();
                (true, true)
            }
            _ => (false, false),
        })
    }

    /// Sets what this computer announces for [`plugin`] (None stops), checked
    /// against the limits a paired device enforces.
    pub fn set_announce(&self, plugin: &str, value: Option<serde_json::Value>) -> Result<(), String> {
        self.reload_settings();
        let mut next = self.settings.lock().unwrap().announce.clone();
        match value {
            Some(v) => next.insert(plugin.to_string(), v),
            None => next.remove(plugin),
        };
        check_announcements(&next)?;
        self.settings.lock().unwrap().announce = next;
        self.save_settings().map_err(|e| e.to_string())
    }

    /// Forgets a device: unpinned, so it can no longer connect.
    pub fn remove_device(&self, peer: &PeerId) -> io::Result<()> {
        TrustStore::update(&self.dirs.trust(), |t| t.revoke(peer)).map_err(|e| io::Error::other(e.to_string()))?;
        self.edit_devices(|devices| {
            devices.retain(|d| d.id != peer.to_hex());
            ((), true)
        });
        Ok(())
    }

    /// Records the global discovery ID a paired device stated in its hello.
    pub fn device_disco(&self, peer: &PeerId, disco: &str) {
        if !droidtop_agent_core::rendezvous::valid_device_id(disco) {
            return;
        }
        self.edit_devices(|devices| match devices.iter_mut().find(|d| d.id == peer.to_hex()) {
            Some(d) if d.disco.as_deref() != Some(disco) => {
                d.disco = Some(disco.to_string());
                ((), true)
            }
            _ => ((), false),
        });
    }

    /// A paired device reached this computer. One the trusted list holds but
    /// the device list lost (the list was emptied by the bug in
    /// Droidtop/tracker#528) is put back, named as it named itself in the
    /// library, so Status and `devices` show it again.
    pub fn seen(&self, peer: &PeerId, address: Option<String>) {
        let id = peer.to_hex();
        let lost = !self.devices_now().iter().any(|d| d.id == id) && self.is_trusted(peer);
        let recovered = lost.then(|| {
            let name = self.library_now().games.values().find_map(|g| g.installs.get(&id).map(|i| i.device_name.clone()));
            name.unwrap_or_else(|| format!("Handheld {}", &id[..8]))
        });
        self.edit_devices(|devices| {
            if devices.iter().all(|d| d.id != id) {
                let Some(name) = recovered else { return ((), false) };
                let paired_ms = droidtop_agent_core::library::now_ms();
                devices.push(Device { id: id.clone(), name, paired_ms, moved: true, ..Default::default() });
            }
            if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
                d.last_seen_ms = droidtop_agent_core::library::now_ms();
                if address.is_some() {
                    d.last_address = address;
                }
            }
            ((), true)
        });
    }

    /// Sets one of the person's marks on a game from this computer (a
    /// favourite, a collection...): it travels to every paired device like
    /// one made on a handheld.
    pub fn set_mark(&self, game: &str, field: &str, value: serde_json::Value) -> bool {
        let device = self.peer_id();
        self.edit_library(|lib| {
            if lib.games.get(game).and_then(|r| r.marks.get(field)).is_some_and(|m| m.value == value) {
                return (false, false);
            }
            let changed = lib.mark(&device, game, field, value);
            (changed, changed)
        })
    }

    /// The person's marks in launchers on this computer (Lutris's favourites
    /// and hidden games) that changed there since the last scan, as this
    /// computer's marks (docs/DESIGN.md section 7). Only a change the launcher
    /// made counts: a mark set on a handheld is not undone because the
    /// launcher still says the old thing, and the agent never writes the
    /// launcher's store.
    pub fn note_launcher_marks(&self, lib: &mut Library, now: droidtop_agent_core::library::Marks) -> usize {
        let path = self.dirs.data.join("launcher-marks.json");
        let before: droidtop_agent_core::library::Marks = read_json(&path).unwrap_or_default();
        let mut changed = droidtop_agent_core::library::Marks::new();
        for (game, fields) in &now {
            for (field, value) in fields {
                if before.get(game).and_then(|f| f.get(field)) != Some(value) {
                    changed.entry(game.clone()).or_default().insert(field.clone(), value.clone());
                }
            }
        }
        let _ = write_json(&path, &now);
        lib.note_marks(&self.peer_id(), &changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    /// The headless agent, the window and a command line are separate
    /// processes with a copy of this state each (Droidtop/tracker#528, #531):
    /// none writes back what it read at the start over what another saved.
    #[test]
    fn copies_of_the_agent_running_together_do_not_overwrite_each_other() {
        let d = dirs("together");
        let serving = Store::open_in(d.clone()).unwrap();
        let window = Store::open_in(d.clone()).unwrap();
        // A handheld is paired by one copy after the other started.
        let handheld = DeviceKey::generate().peer_id();
        window.add_device(handheld, "Handheld", Kind::Handheld).unwrap();
        // The serving copy notes the handheld's visit: it used to write its empty list over the pairing.
        serving.seen(&handheld, Some("10.0.2.15".into()));
        assert_eq!(window.devices_now().len(), 1);
        assert_eq!(serving.devices_now()[0].last_address.as_deref(), Some("10.0.2.15"));
        // A favourite arrives through the window's copy; the serving copy's scan does not lose it.
        assert!(window.set_mark("title:dt sync test", "favourite", serde_json::json!(true)));
        serving.edit_library(|_| ((), true));
        let again = Store::open_in(d).unwrap();
        assert_eq!(again.library_now().games["title:dt sync test"].marks["favourite"].value, serde_json::json!(true));
        assert_eq!(serving.library_now().games["title:dt sync test"].marks.len(), 1);
    }

    /// A second taker waits for the first, however long it holds the lock, and then gets it
    /// (Droidtop/tracker#761: the old lock was broken after ten seconds, live holder or not).
    #[test]
    fn a_lock_is_waited_for_while_it_is_held() {
        let d = dirs("lock");
        let state = d.config.join("state.json");
        let first = FileLock::take(&state).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn({
            let state = state.clone();
            move || {
                let _second = FileLock::take(&state).unwrap();
                tx.send(()).unwrap();
            }
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "taken while the first still held it");
        drop(first);
        rx.recv_timeout(Duration::from_secs(5)).expect("not taken after the first let go");
        waiter.join().unwrap();
    }

    /// A handheld the trusted list holds but the device list lost comes back,
    /// named as the library knows it, when it next reaches the computer.
    #[test]
    fn a_trusted_handheld_missing_from_the_device_list_is_put_back() {
        let d = dirs("lost");
        let agent = Store::open_in(d.clone()).unwrap();
        let handheld = DeviceKey::generate().peer_id();
        TrustStore::update(&d.trust(), |t| t.pin(handheld)).unwrap();
        agent.edit_library(|lib| {
            let game = droidtop_agent_core::library::ScannedGame {
                key: "title:x".into(),
                title: "X".into(),
                platform: None,
                install: droidtop_agent_core::library::Install { installed: true, ..Default::default() },
            };
            ((), lib.update_device(&handheld, "SM-G998B", vec![game]) > 0)
        });
        assert!(agent.devices_now().is_empty());
        agent.seen(&handheld, Some("10.0.2.2".into()));
        let devices = agent.devices_now();
        assert_eq!((devices.len(), devices[0].name.as_str(), devices[0].last_address.as_deref()), (1, "SM-G998B", Some("10.0.2.2")));
        // A device that is not trusted is not added by a visit.
        agent.seen(&DeviceKey::generate().peer_id(), None);
        assert_eq!(agent.devices_now().len(), 1);
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

    #[test]
    fn a_handheld_is_offered_library_and_saves_and_a_computer_nothing() {
        let d = dirs("offers");
        let store = Store::open_in(d.clone()).unwrap();
        let (handheld, computer, old, stranger) = (
            DeviceKey::generate().peer_id(),
            DeviceKey::generate().peer_id(),
            DeviceKey::generate().peer_id(),
            DeviceKey::generate().peer_id(),
        );
        store.add_device(handheld, "Pocket", Kind::Handheld).unwrap();
        store.add_device(computer, "Desk", Kind::Computer).unwrap();
        // One paired before pairing said its kind: a handheld.
        store.add_device(old, "Deck", Kind::Handheld).unwrap();
        store.edit_devices(|ds| {
            ds.iter_mut().find(|x| x.id == old.to_hex()).unwrap().kind = None;
            ((), true)
        });
        assert_eq!(store.offers_to(&handheld), Offer::LIBRARY_AND_SAVES);
        assert_eq!(store.offers_to(&old), Offer::LIBRARY_AND_SAVES);
        assert_eq!(store.offers_to(&computer), Offer::NONE);
        assert_eq!(store.offers_to(&stranger), Offer::NONE);
        assert!(store.syncs_contexts(&handheld) && store.syncs_contexts(&old));
        assert!(!store.syncs_contexts(&computer) && !store.syncs_contexts(&stranger));

        // Saves need the library, both ways.
        store.set_offer(&computer.to_hex(), None, Some(true));
        assert_eq!(store.offers_to(&computer), Offer::LIBRARY_AND_SAVES);
        store.set_offer(&computer.to_hex(), Some(false), None);
        assert_eq!(store.offers_to(&computer), Offer::NONE);
        store.set_offer(&handheld.to_hex(), None, Some(false));
        assert_eq!(store.offers_to(&handheld), Offer { library: true, saves: false });

        // What the person set is kept.
        store.save_settings().unwrap();
        let again = Store::open_in(d).unwrap();
        assert_eq!(again.offers_to(&handheld), Offer { library: true, saves: false });
        assert_eq!(again.offers_to(&computer), Offer::NONE);
    }
}
