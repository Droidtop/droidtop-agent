//! This computer's Syncthing kept in step with the paired devices'
//! (docs/DESIGN.md section 18; Droidtop/tracker#553). droidtop's Syncthing
//! plugin (`droidtop.syncthing`) announces `{"syncthing": "<device ID>",
//! "addresses": [...]}` to every device the person paired with droidtop and
//! accepts exactly the IDs its counterparts announce, so Syncthing's own
//! pairing is never shown. This is the computer's counterpart:
//!
//! - **Which Syncthing** (owner, 2026-10-16): the one already running here
//!   when there is one, otherwise one this agent starts. Either way it is the
//!   Syncthing in the standard location ([`home`]), so the person can move to
//!   or from their own Syncthing with nothing to carry over.
//! - **Announcing:** its device ID and this computer's LAN addresses on its
//!   listening port, under the plugin's id, with `store.set_announce`.
//! - **Accepting:** each paired device's announced ID is added to Syncthing
//!   through its REST API, named as the person named the device, with its
//!   announced addresses plus `dynamic`, trusted, and with its folders
//!   accepted automatically. A device forgotten or no longer announcing is
//!   removed again. Only devices this agent added are ever changed
//!   (`syncthing.json`); devices the person added are never touched.

use std::collections::BTreeMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use droidtop_agent_store::{read_json, write_json};

use crate::Agent;

/// The plugin's id, which is the key its value travels under.
pub const PLUGIN: &str = "droidtop.syncthing";
/// Syncthing's default listening port.
const DEFAULT_PORT: u16 = 22000;

/// The standard Syncthing home on this system: `STHOMEDIR` when set, else
/// Syncthing's own default for the platform. On Linux that is
/// `$XDG_STATE_HOME/syncthing` (`~/.local/state/syncthing`) unless an older
/// install's `~/.config/syncthing` holds a configuration, which Syncthing
/// itself still uses.
pub fn home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("STHOMEDIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::data_local_dir)
            .map(|d| d.join("Syncthing"));
    }
    if cfg!(target_os = "macos") {
        return dirs::home_dir().map(|h| h.join("Library/Application Support/Syncthing"));
    }
    let old = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|d| d.join("syncthing"));
    if let Some(old) = old.filter(|d| d.join("config.xml").is_file()) {
        return Some(old);
    }
    std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .map(|d| d.join("syncthing"))
}

/// What this agent needs from a Syncthing home's configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// This Syncthing's device ID, from its certificate.
    pub device_id: String,
    /// Where its REST API answers, as `http://host:port`.
    pub api: String,
    pub api_key: String,
    /// Its listening port for other devices.
    pub port: u16,
}

/// Reads `config.xml` and `cert.pem` in `home`.
pub fn read_config(home: &Path) -> Result<Config, String> {
    let text = fs::read_to_string(home.join("config.xml")).map_err(|e| format!("Syncthing's config.xml: {e}"))?;
    let doc = roxmltree::Document::parse(&text).map_err(|e| format!("Syncthing's config.xml: {e}"))?;
    let gui = doc.descendants().find(|n| n.has_tag_name("gui")).ok_or("Syncthing's config.xml has no gui section")?;
    let child =
        |n: roxmltree::Node, name: &str| n.children().find(|c| c.has_tag_name(name)).and_then(|c| c.text()).map(|t| t.trim().to_string());
    if gui.attribute("tls") == Some("true") {
        return Err("Syncthing's web interface uses HTTPS, which this agent cannot reach yet".into());
    }
    let address = child(gui, "address").unwrap_or_else(|| "127.0.0.1:8384".into());
    if address.starts_with("unix") {
        return Err("Syncthing's web interface listens on a Unix socket, which this agent cannot reach yet".into());
    }
    let api_key = child(gui, "apikey").filter(|k| !k.is_empty()).ok_or("Syncthing's config.xml has no API key")?;
    let port = doc
        .descendants()
        .filter(|n| n.has_tag_name("listenAddress"))
        .filter_map(|n| n.text())
        .find_map(|a| listen_port(a.trim()))
        .unwrap_or(DEFAULT_PORT);
    let cert = fs::read_to_string(home.join("cert.pem")).map_err(|e| format!("Syncthing's cert.pem: {e}"))?;
    Ok(Config { device_id: device_id(&pem_der(&cert)?), api: format!("http://{}", loopback(&address)), api_key, port })
}

/// The port in a listen address (`tcp://0.0.0.0:22000`); `default` is 22000.
fn listen_port(address: &str) -> Option<u16> {
    if address == "default" {
        return Some(DEFAULT_PORT);
    }
    let rest = address.strip_prefix("tcp://").or_else(|| address.strip_prefix("quic://"))?;
    rest.rsplit(':').next()?.parse().ok()
}

/// A GUI address listening on every interface is reached on loopback.
fn loopback(address: &str) -> String {
    for any in ["0.0.0.0:", "[::]:", ":"] {
        if let Some(port) = address.strip_prefix(any) {
            return format!("127.0.0.1:{port}");
        }
    }
    address.to_string()
}

/// The DER bytes of the first certificate in a PEM file.
fn pem_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String =
        pem.lines().skip_while(|l| !l.starts_with("-----BEGIN CERTIFICATE")).skip(1).take_while(|l| !l.starts_with("-----END")).collect();
    if body.is_empty() {
        return Err("Syncthing's cert.pem holds no certificate".into());
    }
    base64_decode(&body).ok_or_else(|| "Syncthing's cert.pem is not valid base64".into())
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = (acc << 6) | ALPHABET.iter().position(|&a| a == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Syncthing's device ID of a certificate, as the rendezvous computes its
/// own (windowcast-rendezvous).
pub fn device_id(der: &[u8]) -> String {
    droidtop_agent_core::rendezvous::device_id(der)
}

/// Whether text is a Syncthing device ID as droidtop's plugin accepts one.
pub fn is_device_id(text: &str) -> bool {
    text.len() == 63 && text.split('-').count() == 8 && text.split('-').all(|g| g.len() == 7 && g.bytes().all(|b| BASE32.contains(&b)))
}

/// What this computer announces: its ID and its LAN addresses on `port`.
pub fn announcement(config: &Config, ips: &[IpAddr]) -> Value {
    let mut addresses = Vec::new();
    for ip in ips {
        let host = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        addresses.push(format!("tcp://{host}:{}", config.port));
        addresses.push(format!("quic://{host}:{}", config.port));
    }
    json!({ "syncthing": config.device_id, "addresses": addresses })
}

/// This computer's LAN addresses: IPv4 and IPv6 that are not loopback or link-local.
fn lan_ips() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
            IpAddr::V6(v6) => !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80 && !v6.is_unspecified(),
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

/// A device to keep in Syncthing for a paired device.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub id: String,
    pub name: String,
    pub addresses: Vec<String>,
}

/// The Syncthing devices the paired devices announced: each valid ID once,
/// never this Syncthing's own.
pub fn wanted(devices: &[droidtop_agent_store::Device], own: &str) -> Vec<Wanted> {
    let mut out: Vec<Wanted> = Vec::new();
    for d in devices {
        let Some(value) = d.announced.get(PLUGIN) else { continue };
        let Some(id) = value.get("syncthing").and_then(Value::as_str).map(|s| s.trim().to_uppercase()) else { continue };
        if !is_device_id(&id) || id == own || out.iter().any(|w| w.id == id) {
            continue;
        }
        let mut addresses: Vec<String> = value
            .get("addresses")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| s.starts_with("tcp://") || s.starts_with("quic://"))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        addresses.push("dynamic".into());
        out.push(Wanted { id, name: d.name.clone(), addresses });
    }
    out
}

/// The Syncthing devices this agent added, by Syncthing ID (`syncthing.json`).
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Added {
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
}

/// Syncthing's REST API, as far as this agent uses it.
pub trait Api {
    /// The device IDs in Syncthing's configuration.
    fn devices(&self) -> Result<Vec<String>, String>;
    /// Adds or replaces a device (Syncthing's defaults for the rest).
    fn put_device(&self, w: &Wanted) -> Result<(), String>;
    fn delete_device(&self, id: &str) -> Result<(), String>;
}

/// What changed in one pass: `added` and `removed` device IDs.
#[derive(Debug, Default, PartialEq)]
pub struct Pass {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// Brings Syncthing's devices to `wanted`, changing only devices in `added`
/// (and recording the ones it adds there). A device the person had already
/// added is left as they set it.
pub fn reconcile(api: &dyn Api, wanted: &[Wanted], added: &mut Added) -> Result<Pass, String> {
    let present = api.devices()?;
    let mut pass = Pass::default();
    for w in wanted {
        let ours = added.devices.contains_key(&w.id);
        if present.contains(&w.id) && !ours {
            continue;
        }
        if !present.contains(&w.id) {
            pass.added.push(w.id.clone());
        }
        api.put_device(w)?;
        added.devices.insert(w.id.clone(), w.name.clone());
    }
    let gone: Vec<String> = added.devices.keys().filter(|id| !wanted.iter().any(|w| &w.id == *id)).cloned().collect();
    for id in gone {
        if present.contains(&id) {
            api.delete_device(&id)?;
            pass.removed.push(id.clone());
        }
        added.devices.remove(&id);
    }
    Ok(pass)
}

/// Syncthing's REST API over HTTP on loopback.
pub struct Rest {
    pub config: Config,
    agent: ureq::Agent,
}

impl Rest {
    pub fn new(config: Config) -> Rest {
        Rest { config, agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.config.api)
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        self.agent
            .get(&self.url(path))
            .set("X-API-Key", &self.config.api_key)
            .call()
            .map_err(|e| format!("Syncthing {path}: {e}"))?
            .into_json()
            .map_err(|e| format!("Syncthing {path}: {e}"))
    }

    /// Whether Syncthing answers.
    pub fn ping(&self) -> bool {
        self.get("/rest/system/ping").is_ok()
    }
}

impl Api for Rest {
    fn devices(&self) -> Result<Vec<String>, String> {
        let list = self.get("/rest/config/devices")?;
        Ok(list
            .as_array()
            .map(|a| a.iter().filter_map(|d| d.get("deviceID").and_then(Value::as_str).map(str::to_string)).collect())
            .unwrap_or_default())
    }

    fn put_device(&self, w: &Wanted) -> Result<(), String> {
        let mut device = self.get("/rest/config/defaults/device").unwrap_or_else(|_| json!({}));
        device["deviceID"] = json!(w.id);
        device["name"] = json!(w.name);
        device["addresses"] = json!(w.addresses);
        device["autoAcceptFolders"] = json!(true);
        device["untrusted"] = json!(false);
        device["introducer"] = json!(false);
        let path = format!("/rest/config/devices/{}", w.id);
        self.agent
            .put(&self.url(&path))
            .set("X-API-Key", &self.config.api_key)
            .send_json(device)
            .map(|_| ())
            .map_err(|e| format!("Syncthing {path}: {e}"))
    }

    fn delete_device(&self, id: &str) -> Result<(), String> {
        let path = format!("/rest/config/devices/{id}");
        self.agent
            .delete(&self.url(&path))
            .set("X-API-Key", &self.config.api_key)
            .call()
            .map(|_| ())
            .map_err(|e| format!("Syncthing {path}: {e}"))
    }
}

/// The Syncthing program: `DROIDTOP_SYNCTHING`, one beside this program,
/// or one on the PATH.
pub fn program() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DROIDTOP_SYNCTHING").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let name = if cfg!(windows) { "syncthing.exe" } else { "syncthing" };
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name))).filter(|p| p.is_file());
    beside.or_else(|| std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())))
}

/// The Syncthing this agent started, when it started one.
static STARTED: Mutex<Option<Child>> = Mutex::new(None);

/// Starts Syncthing over `home` with no browser and no monitor process (a
/// pass starts it again if it stopped), and waits up to `wait` for
/// its configuration and API. Syncthing makes `home` the first time.
fn start(program: &Path, home: &Path, wait: Duration) -> Result<Rest, String> {
    let mut started = STARTED.lock().unwrap();
    if let Some(child) = started.as_mut() {
        if child.try_wait().ok().flatten().is_some() {
            *started = None;
        }
    }
    if started.is_none() {
        let mut cmd = Command::new(program);
        cmd.arg("serve")
            .arg("--no-browser")
            .arg("--no-restart")
            .arg(format!("--home={}", home.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        *started = Some(cmd.spawn().map_err(|e| format!("Syncthing ({}) did not start: {e}", program.display()))?);
    }
    drop(started);
    let until = Instant::now() + wait;
    loop {
        if let Ok(config) = read_config(home) {
            let rest = Rest::new(config);
            if rest.ping() {
                return Ok(rest);
            }
        }
        if Instant::now() > until {
            return Err("Syncthing started but its API did not answer".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// What the last pass found, so a problem is reported once, not every pass.
static LAST: Mutex<Option<String>> = Mutex::new(None);

fn report(agent: &Agent, outcome: Result<Pass, String>) {
    let text = match &outcome {
        Ok(_) => "ok".to_string(),
        Err(e) => e.clone(),
    };
    let mut last = LAST.lock().unwrap();
    match outcome {
        Ok(pass) => {
            for id in &pass.added {
                agent.events.info("syncthing", format!("Syncthing: added {id} for a paired device."), json!({ "syncthing": id }));
            }
            for id in &pass.removed {
                agent.events.info(
                    "syncthing",
                    format!("Syncthing: removed {id}; its device is no longer paired or announcing."),
                    json!({ "syncthing": id }),
                );
            }
        }
        Err(e) if last.as_deref() != Some(text.as_str()) => agent.events.warn("syncthing", format!("Syncthing: {e}"), json!({})),
        Err(_) => {}
    }
    *last = Some(text);
}

/// One pass: find or start this computer's Syncthing, announce its ID, and
/// bring its devices to what the paired devices announced. With the setting
/// off it withdraws the announcement and changes nothing in Syncthing.
pub fn tick(agent: &Agent) {
    let on = agent.store.settings.lock().unwrap().syncthing;
    if !on {
        if agent.store.settings.lock().unwrap().announce.contains_key(PLUGIN) {
            let _ = agent.store.set_announce(PLUGIN, None);
        }
        return;
    }
    report(agent, pass(agent));
}

fn pass(agent: &Agent) -> Result<Pass, String> {
    let home = home().ok_or("this system has no folder for Syncthing")?;
    let running = read_config(&home).ok().map(Rest::new).filter(Rest::ping);
    let rest = match running {
        Some(rest) => rest,
        None => {
            let program = program().ok_or("Syncthing is not installed on this computer")?;
            start(&program, &home, Duration::from_secs(60))?
        }
    };
    let value = announcement(&rest.config, &lan_ips());
    if agent.store.settings.lock().unwrap().announce.get(PLUGIN) != Some(&value) {
        agent.store.set_announce(PLUGIN, Some(value))?;
    }
    let path = agent.store.dirs.syncthing();
    let mut added: Added = read_json(&path).map_err(|e| e.to_string())?;
    let devices: Vec<_> = agent
        .store
        .devices_now()
        .into_iter()
        .filter(|d| droidtop_agent_core::keys::PeerId::from_hex(&d.id).map(|p| agent.store.is_trusted(&p)).unwrap_or(false))
        .collect();
    let result = reconcile(&rest, &wanted(&devices, &rest.config.device_id), &mut added);
    write_json(&path, &added).map_err(|e| e.to_string())?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn device_ids_are_recognised_as_the_plugin_accepts_them() {
        // lib/protocol/deviceid_test.go's formatted ID.
        assert!(is_device_id("P56IOI7-MZJNU2Y-IQGDREY-DM2MGTI-MGL3BXN-PQ6W5BM-TBBZ4TJ-XZWICQ2"));
        assert!(!is_device_id("PC-ID"));
    }

    #[test]
    fn a_device_id_is_the_certificates_digest() {
        let id = device_id(b"not really DER");
        assert!(is_device_id(&id), "{id}");
        assert_eq!(id, device_id(b"not really DER"));
        assert_ne!(id, device_id(b"another"));
    }

    #[test]
    fn the_config_gives_the_api_key_address_port_and_id() {
        let dir = std::env::temp_dir().join(format!("dta-syncthing-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.xml"),
            r#"<configuration version="37"><gui enabled="true" tls="false"><address>0.0.0.0:8385</address><apikey>KEY</apikey></gui><options><listenAddress>tcp://0.0.0.0:22001</listenAddress></options></configuration>"#,
        )
        .unwrap();
        fs::write(dir.join("cert.pem"), "-----BEGIN CERTIFICATE-----\nbm90IHJlYWxseSBERVI=\n-----END CERTIFICATE-----\n").unwrap();
        let c = read_config(&dir).unwrap();
        assert_eq!(c.api, "http://127.0.0.1:8385");
        assert_eq!(c.api_key, "KEY");
        assert_eq!(c.port, 22001);
        assert_eq!(c.device_id, device_id(b"not really DER"));
        let value = announcement(&c, &["192.168.1.5".parse().unwrap()]);
        assert_eq!(value, json!({ "syncthing": c.device_id, "addresses": ["tcp://192.168.1.5:22001", "quic://192.168.1.5:22001"] }));
    }

    struct Fake {
        present: RefCell<Vec<String>>,
        calls: RefCell<Vec<String>>,
    }

    impl Api for Fake {
        fn devices(&self) -> Result<Vec<String>, String> {
            Ok(self.present.borrow().clone())
        }
        fn put_device(&self, w: &Wanted) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("put {} {} {:?}", w.id, w.name, w.addresses));
            if !self.present.borrow().contains(&w.id) {
                self.present.borrow_mut().push(w.id.clone());
            }
            Ok(())
        }
        fn delete_device(&self, id: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("delete {id}"));
            self.present.borrow_mut().retain(|p| p != id);
            Ok(())
        }
    }

    fn id(c: char) -> String {
        device_id(&[c as u8])
    }

    fn paired(name: &str, value: Option<Value>) -> droidtop_agent_store::Device {
        let mut d: droidtop_agent_store::Device = serde_json::from_value(json!({ "id": name, "name": name, "paired_ms": 1 })).unwrap();
        if let Some(v) = value {
            d.announced.insert(PLUGIN.into(), v);
        }
        d
    }

    #[test]
    fn paired_devices_ids_are_added_and_removed_and_the_persons_are_left_alone() {
        let (deck, win4, own, persons) = (id('A'), id('B'), id('C'), id('D'));
        let fake = Fake { present: RefCell::new(vec![own.clone(), persons.clone()]), calls: RefCell::new(vec![]) };
        let devices = vec![
            paired("Deck", Some(json!({ "syncthing": deck.to_lowercase(), "addresses": ["tcp://10.0.0.2:22000", "http://x"] }))),
            paired("Win 4", Some(json!({ "syncthing": win4 }))),
            paired("Mine", Some(json!({ "syncthing": own }))),
            paired("Theirs", Some(json!({ "syncthing": persons }))),
            paired("Quiet", None),
            paired("Bad", Some(json!({ "syncthing": "PC-ID" }))),
        ];
        let mut added = Added::default();
        let pass = reconcile(&fake, &wanted(&devices, &own), &mut added).unwrap();
        assert_eq!(pass.added, vec![deck.clone(), win4.clone()]);
        assert!(fake.calls.borrow()[0].contains(r#"["tcp://10.0.0.2:22000", "dynamic"]"#));
        assert!(!added.devices.contains_key(&persons), "a device the person added stays theirs");

        // The Win 4 is forgotten: only it goes, and the person's device stays.
        fake.calls.borrow_mut().clear();
        let pass = reconcile(&fake, &wanted(&devices[..1], &own), &mut added).unwrap();
        assert_eq!(pass.removed, vec![win4.clone()]);
        assert!(fake.present.borrow().contains(&persons));
        assert_eq!(added.devices.keys().cloned().collect::<Vec<_>>(), vec![deck]);
    }

    /// Against a real Syncthing when `DROIDTOP_SYNCTHING` names one (CI can
    /// set it): started over a fresh home, its ID read from cert.pem is its
    /// own `myID`, and a paired device is added and removed through the API.
    #[test]
    fn a_real_syncthing_is_started_read_and_kept_in_step() {
        let Some(program) = std::env::var_os("DROIDTOP_SYNCTHING").map(PathBuf::from) else { return };
        let home = std::env::temp_dir().join(format!("dta-syncthing-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        std::env::set_var("STNODEFAULTFOLDER", "1");
        let rest = start(&program, &home, Duration::from_secs(60)).unwrap();
        let status = rest.get("/rest/system/status").unwrap();
        assert_eq!(status["myID"].as_str().unwrap(), rest.config.device_id);

        let deck = device_id(b"a deck's certificate");
        let mut added = Added::default();
        let wanted =
            vec![Wanted { id: deck.clone(), name: "Deck".into(), addresses: vec!["tcp://10.0.0.2:22000".into(), "dynamic".into()] }];
        assert_eq!(reconcile(&rest, &wanted, &mut added).unwrap().added, vec![deck.clone()]);
        let device = rest.get(&format!("/rest/config/devices/{deck}")).unwrap();
        assert_eq!(device["name"], "Deck");
        assert_eq!(device["autoAcceptFolders"], true);
        assert!(rest.devices().unwrap().contains(&rest.config.device_id), "its own device is left alone");
        assert_eq!(reconcile(&rest, &[], &mut added).unwrap().removed, vec![deck.clone()]);
        assert!(!rest.devices().unwrap().contains(&deck));

        if let Some(mut child) = STARTED.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&home);
    }
}
