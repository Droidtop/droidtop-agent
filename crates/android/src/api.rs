//! The operations droidtop calls, JSON in and JSON out, so the Kotlin side
//! needs no types of its own beyond reading the replies. A failure comes back
//! as `{"error": "..."}` in words the screens can show; a computer that
//! could not be reached as `{"unreachable": "..."}`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::context::{baseline_when_deferred, merge, ContextDecl, Records};
use droidtop_agent_core::discovery::{self, Announce};
use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::{Cursor, Library, Marks, ScannedGame};
use droidtop_agent_core::mailbox::{self, Envelope};
use droidtop_agent_core::pairing::{self, PairInvite};
use droidtop_agent_core::proto::{GameRef, Request, Response};
use droidtop_agent_core::saves::{prefix_roots, Roots};
use droidtop_agent_core::savesync::{self, SaveSyncRequest, Side};
use droidtop_agent_core::tunnel::{self, TunnelStream};
use droidtop_agent_core::{hex, Error, PAIR_PORT, PORT, PROTOCOL_VERSION};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn error(message: &str) -> String {
    json!({ "error": message }).to_string()
}

pub fn call(op: &str, args: &str) -> String {
    let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let result = match op {
        "identity_new" => Ok(identity_new()),
        "identity_id" => identity_id(&args),
        "pair_start" => pair_start(&args),
        "pair_wait" => pair_wait(&args),
        "pair_connect" => pair_connect(&args),
        "pair_cancel" => {
            CANCEL.store(true, Ordering::Relaxed);
            Ok(json!({}))
        }
        "find" => find(&args),
        "sync_saves" => sync_saves(&args),
        "sync_library" => sync_library(&args),
        "sync_context" => sync_context(&args),
        "share_library" => share_library(&args),
        "share_post_saves" => share_post_saves(&args),
        other => Err(Failure::Error(format!("unknown operation {other}"))),
    };
    match result {
        Ok(v) => v.to_string(),
        Err(Failure::Error(m)) => error(&m),
        Err(Failure::Unreachable(m)) => json!({ "unreachable": m }).to_string(),
    }
}

enum Failure {
    Error(String),
    Unreachable(String),
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Failure::Error(e.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(e: serde_json::Error) -> Self {
        Failure::Error(format!("droidtop sent arguments the agent library cannot read ({e})"))
    }
}

type Outcome = Result<Value, Failure>;

fn key_of(args: &Value) -> Result<DeviceKey, Failure> {
    let seed = args["seed"].as_str().and_then(hex::decode).ok_or_else(|| Failure::Error("no device key was given".into()))?;
    Ok(DeviceKey::from_seed(&seed)?)
}

fn peer_of(text: &str) -> Result<PeerId, Failure> {
    PeerId::from_hex(text).map_err(|_| Failure::Error("that computer's id is not valid".into()))
}

fn identity_new() -> Value {
    let key = DeviceKey::generate();
    json!({ "seed": hex::encode(&key.seed()), "id": key.peer_id().to_hex() })
}

fn identity_id(args: &Value) -> Outcome {
    Ok(json!({ "id": key_of(args)?.peer_id().to_hex() }))
}

// Pairing --------------------------------------------------------------------

struct PairSession {
    listener: TcpListener,
    code: String,
    key: DeviceKey,
    name: String,
    stop: Arc<AtomicBool>,
}

static SESSION: Mutex<Option<PairSession>> = Mutex::new(None);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Wrong codes a pairing screen accepts before it ends its session.
const MAX_ATTEMPTS: u32 = 3;

/// Opens the pairing listener for as long as the pairing screen is open,
/// and answers the computer's pairing query on the LAN.
fn pair_start(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let name = args["name"].as_str().unwrap_or("droidtop").to_string();
    if let Some(old) = SESSION.lock().unwrap().take() {
        old.stop.store(true, Ordering::Relaxed);
    }
    CANCEL.store(false, Ordering::Relaxed);
    let listener = TcpListener::bind(("0.0.0.0", 0)).map_err(|e| Failure::Error(format!("could not open a pairing port ({e})")))?;
    let port = listener.local_addr().map_err(|e| Failure::Error(e.to_string()))?.port();
    let code = pairing::new_code();
    let stop = Arc::new(AtomicBool::new(false));
    let announce = Announce { id: key.peer_id().to_hex(), name: name.clone(), port, sig: String::new() };
    // The UDP answer is a convenience: without it the computer still pairs
    // with the address in the QR code's text.
    if let Ok(sock) = UdpSocket::bind(("0.0.0.0", PORT)) {
        let stop = stop.clone();
        thread::spawn(move || {
            let _ = discovery::answer_pairing(&sock, &announce, &stop);
        });
    }
    let at = args["address"].as_str().filter(|a| !a.is_empty()).map(|a| format!("{a}:{port}"));
    let invite = PairInvite { code: code.clone(), peer: Some(key.peer_id()), name: Some(name.clone()), at };
    let uri = invite.to_uri();
    *SESSION.lock().unwrap() = Some(PairSession { listener, code: code.clone(), key, name, stop });
    Ok(json!({ "code": code, "uri": uri, "port": port }))
}

/// Waits for a computer to pair, up to `timeout_ms`, or until cancelled.
fn pair_wait(args: &Value) -> Outcome {
    let timeout = Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(600_000));
    let session = SESSION.lock().unwrap().take().ok_or_else(|| Failure::Error("pairing was not started".into()))?;
    let end = |s: &PairSession| s.stop.store(true, Ordering::Relaxed);
    session.listener.set_nonblocking(true).map_err(|e| Failure::Error(e.to_string()))?;
    let deadline = Instant::now() + timeout;
    let mut attempts = 0;
    while Instant::now() < deadline && !CANCEL.load(Ordering::Relaxed) {
        match session.listener.accept() {
            Ok((mut stream, from)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                match pairing::pair_host(&mut stream, &session.key, &session.name, &session.code) {
                    Ok(paired) => {
                        end(&session);
                        return Ok(
                            json!({ "peer": paired.peer.to_hex(), "name": paired.name, "address": format!("{}:{PORT}", from.ip()) }),
                        );
                    }
                    Err(e) => {
                        attempts += 1;
                        if attempts >= MAX_ATTEMPTS {
                            end(&session);
                            return Err(Failure::Error(format!("pairing stopped after {MAX_ATTEMPTS} wrong codes ({e})")));
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                end(&session);
                return Err(Failure::Error(e.to_string()));
            }
        }
    }
    end(&session);
    Err(Failure::Error(if CANCEL.load(Ordering::Relaxed) { "pairing was cancelled".into() } else { "no computer paired in time".into() }))
}

/// Pairing the other way round: the computer shows its address and a code
/// (`droidtop-agent pair` with nothing after it), and this device connects.
/// For a handheld the computer cannot reach, such as one behind an
/// emulator's or a guest network's NAT. The computer is then reached at the
/// same address on the agent's port.
fn pair_connect(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let name = args["name"].as_str().unwrap_or("droidtop").to_string();
    let invite = PairInvite::parse(args["code"].as_str().unwrap_or_default())
        .ok_or_else(|| Failure::Error("the code is the 6 digits the computer shows".into()))?;
    let typed = args["address"].as_str().map(str::trim).unwrap_or_default();
    let at = invite.at.as_deref().unwrap_or(typed);
    let address: SocketAddr = at
        .parse()
        .or_else(|_| format!("{at}:{PAIR_PORT}").parse())
        .map_err(|_| Failure::Error(format!("{at} is not an address such as 192.168.1.20 or 192.168.1.20:{PAIR_PORT}")))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|e| Failure::Unreachable(format!("the computer did not answer at {address} ({e})")))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let paired = pairing::pair_client(&mut stream, &key, &name, &invite.code)?;
    if invite.peer.is_some_and(|p| p != paired.peer) {
        return Err(Failure::Error("the computer that answered is not the one in the invitation; nothing was paired".into()));
    }
    Ok(json!({ "peer": paired.peer.to_hex(), "name": paired.name, "address": SocketAddr::new(address.ip(), PORT).to_string() }))
}

// Reaching a paired computer -------------------------------------------------

fn find(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let peers: Vec<PeerId> = args["peers"]
        .as_array()
        .map(|a| a.iter().filter_map(|p| p.as_str()).filter_map(|p| PeerId::from_hex(p).ok()).collect())
        .unwrap_or_default();
    let timeout = Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(1500));
    let found = discovery::find_agents(&key.peer_id(), &peers, timeout).map_err(|e| Failure::Error(e.to_string()))?;
    Ok(
        json!({ "found": found.iter().map(|f| json!({ "id": f.peer.to_hex(), "name": f.name, "address": f.addr.to_string() })).collect::<Vec<_>>() }),
    )
}

fn parse_address(text: &str) -> Option<SocketAddr> {
    text.parse().ok().or_else(|| format!("{text}:{PORT}").parse().ok())
}

/// The stream a session runs on: TCP on the LAN, or TCP inside a WireGuard tunnel.
pub enum Link {
    Tcp(TcpStream),
    Tunnel(TunnelStream),
}

impl Read for Link {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Link::Tcp(s) => s.read(buf),
            Link::Tunnel(s) => s.read(buf),
        }
    }
}

impl Write for Link {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Link::Tcp(s) => s.write(buf),
            Link::Tunnel(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Link::Tcp(s) => s.flush(),
            Link::Tunnel(s) => s.flush(),
        }
    }
}

/// A session with the computer: where it answered, its name and its WireGuard endpoints.
struct Session {
    ch: Channel<Link>,
    address: String,
    computer: String,
    endpoints: Vec<String>,
}

fn hello(mut ch: Channel<Link>, address: String, args: &Value) -> Result<Session, Failure> {
    let name = args["name"].as_str().unwrap_or("droidtop").to_string();
    let features = droidtop_agent_core::FEATURES.iter().map(|f| f.to_string()).collect();
    ch.send_json(&Request::Hello { name, version: PROTOCOL_VERSION, features })?;
    let (computer, endpoints) = match ch.recv_json::<Response>()? {
        Response::Hello { name, endpoints, .. } => (name, endpoints),
        _ => (String::new(), Vec::new()),
    };
    Ok(Session { ch, address, computer, endpoints })
}

/// A session with the computer, in the design's order (docs/DESIGN.md
/// section 10): its known LAN addresses, then the LAN by broadcast, then its
/// WireGuard endpoints (`wg:<ip>:<port>`).
fn connect(key: &DeviceKey, args: &Value) -> Result<Session, Failure> {
    let peer = peer_of(args["peer"].as_str().unwrap_or_default())?;
    let known: Vec<String> =
        args["addresses"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let mut candidates: Vec<SocketAddr> = known.iter().filter(|a| !a.starts_with("wg:")).filter_map(|a| parse_address(a)).collect();
    let tunnels: Vec<SocketAddr> = known.iter().filter_map(|a| a.strip_prefix("wg:")).filter_map(|a| a.parse().ok()).collect();
    let mut tried_lan = false;
    let mut last = String::from("it did not answer on this network");
    loop {
        for addr in std::mem::take(&mut candidates) {
            let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(3)) else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));
            match Channel::connect(Link::Tcp(stream), key, &peer) {
                Ok(ch) => return hello(ch, addr.to_string(), args),
                Err(e) => last = e.to_string(),
            }
        }
        if tried_lan {
            break;
        }
        tried_lan = true;
        if let Ok(found) = discovery::find_agents(&key.peer_id(), &[peer], Duration::from_millis(1500)) {
            candidates = found.into_iter().map(|f| f.addr).collect();
        }
    }
    if !tunnels.is_empty() {
        match tunnel::connect(key, &peer, &tunnels, Duration::from_secs(8)) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(120)));
                let ch = Channel::connect(Link::Tunnel(stream), key, &peer)?;
                return hello(ch, String::new(), args);
            }
            Err(e) => last = format!("{last}; through WireGuard: {e}"),
        }
    }
    Err(Failure::Unreachable(last))
}

fn bye(mut ch: Channel<Link>) {
    let _ = ch.send_json(&Request::Bye);
}

/// Adds where the computer answered, its name and its endpoints to a reply.
fn located(mut v: Value, s: &Session) -> Value {
    if !s.address.is_empty() {
        v["address"] = json!(s.address);
    }
    v["computer"] = json!(s.computer);
    v["endpoints"] = json!(s.endpoints);
    v
}

// Saves ----------------------------------------------------------------------

#[derive(Deserialize)]
struct SavesArgs {
    game: GameRef,
    /// The game's Wine prefix (`drive_c`) and Windows user, when it runs in one.
    #[serde(default)]
    prefix: Option<PathBuf>,
    #[serde(default)]
    user: Option<String>,
    /// The game's folder.
    #[serde(default)]
    base: Option<PathBuf>,
    /// Folders for further tokens, as the caller resolves them.
    #[serde(default)]
    roots: Roots,
    baseline: PathBuf,
    archive: PathBuf,
    #[serde(default)]
    choice: Option<Side>,
}

impl SavesArgs {
    /// This device's folder for each token, for this game.
    fn roots(&self) -> Roots {
        let mut roots = match (&self.prefix, &self.user) {
            (Some(prefix), Some(user)) => prefix_roots(prefix, user),
            _ => Roots::new(),
        };
        if let Some(base) = &self.base {
            roots.insert("<base>".into(), base.clone());
        }
        roots.extend(self.roots.clone());
        roots
    }
}

fn sync_saves(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let a: SavesArgs = serde_json::from_value(args.clone())?;
    let roots = a.roots();
    let mut s = connect(&key, args)?;
    let request =
        SaveSyncRequest { game: a.game.clone(), roots: &roots, baseline_path: &a.baseline, archive_dir: &a.archive, choice: a.choice };
    let outcome = savesync::sync(&mut s.ch, &request);
    let v = located(serde_json::to_value(outcome?)?, &s);
    bye(s.ch);
    Ok(v)
}

// Library --------------------------------------------------------------------

/// This device's side of a library exchange, read before anything travels:
/// what it has installed and the marks it keeps on those games (favourite,
/// hidden, completed), recorded in the core's library file.
struct LocalLibrary {
    lib: Library,
    state: PathBuf,
    marks: Marks,
}

impl LocalLibrary {
    fn read(key: &DeviceKey, args: &Value) -> Result<LocalLibrary, Failure> {
        let me = key.peer_id();
        let state = PathBuf::from(args["state"].as_str().ok_or_else(|| Failure::Error("no library file was given".into()))?);
        let name = args["name"].as_str().unwrap_or("droidtop").to_string();
        let scan: Vec<ScannedGame> = serde_json::from_value(args["scan"].clone()).unwrap_or_default();
        let marks: Marks = serde_json::from_value(args["marks"].clone()).unwrap_or_default();
        let mut lib = Library::load(&state).map_err(|e| Failure::Error(e.to_string()))?;
        lib.update_device(&me, &name, scan);
        lib.note_marks(&me, &marks);
        lib.save(&state).map_err(|e| Failure::Error(e.to_string()))?;
        Ok(LocalLibrary { lib, state, marks })
    }

    fn save(&self) -> Result<(), Failure> {
        self.lib.save(&self.state).map_err(|e| Failure::Error(e.to_string()))
    }

    /// The marks that arrived from elsewhere, for droidtop to write into its own library.
    fn to_write(&self) -> Value {
        json!(self.lib.marks_to_write(&self.marks))
    }
}

fn sync_library(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let peer_hex = args["peer"].as_str().unwrap_or_default().to_string();
    let mut local = LocalLibrary::read(&key, args)?;
    let mut s = connect(&key, args)?;
    let lib = &mut local.lib;
    let ch = &mut s.ch;
    let cursor: Cursor = lib.cursors.get(&peer_hex).copied().unwrap_or_default();
    let (outgoing, seq) = lib.changes_since(cursor.pushed);
    let pushed = outgoing.len();
    if !outgoing.is_empty() {
        ch.send_json(&Request::LibraryPush { changes: outgoing })?;
        match ch.recv_json::<Response>()? {
            Response::Ok => {}
            Response::Error { message } => return Err(Failure::Error(message)),
            other => return Err(Failure::Error(format!("the computer answered {other:?}"))),
        }
    }
    ch.send_json(&Request::LibraryPull { since: cursor.pulled })?;
    let (incoming, their) = match ch.recv_json::<Response>()? {
        Response::LibraryChanges { changes, cursor } => (changes, cursor),
        Response::Error { message } => return Err(Failure::Error(message)),
        other => return Err(Failure::Error(format!("the computer answered {other:?}"))),
    };
    let pulled = incoming.iter().filter(|c| lib.apply((*c).clone())).count();
    lib.cursors.insert(peer_hex, Cursor { pulled: their, pushed: seq });
    local.save()?;
    let v = located(json!({ "pushed": pushed, "pulled": pulled, "marks": local.to_write() }), &s);
    bye(s.ch);
    Ok(v)
}

// The person's own cloud share ----------------------------------------------
//
// droidtop reaches the share through Android's document picker, which the
// core cannot open, so it keeps two folders of its own that mirror the
// share's layout (`droidtop-agent/<recipient id>/inbox/`): `inbox`, where it
// copies what the share holds for this device before the call, and
// `outbox`, which it copies into the share after it and empties. The core
// seals, opens and applies; droidtop only moves files.

fn path_arg(args: &Value, name: &str) -> Result<PathBuf, Failure> {
    args[name].as_str().filter(|p| !p.is_empty()).map(PathBuf::from).ok_or_else(|| Failure::Error(format!("no {name} folder was given")))
}

/// The library through the share: what the computer left is applied, and
/// what it has not had yet is left for it. Letters from anyone but this
/// computer stay where they are.
fn share_library(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let peer = peer_of(args["peer"].as_str().unwrap_or_default())?;
    let inbox = path_arg(args, "inbox")?;
    let outbox = path_arg(args, "outbox")?;
    let mut local = LocalLibrary::read(&key, args)?;
    let (letters, _) = mailbox::collect(&inbox, &key, |p| *p == peer).map_err(|e| Failure::Error(e.to_string()))?;
    let mut applied = 0;
    let mut done = Vec::new();
    let mut refused = Vec::new();
    for letter in letters {
        match mailbox::unpack(&letter.payload) {
            Ok((Envelope::Library { changes }, _)) => applied += changes.into_iter().filter(|c| local.lib.apply(c.clone())).count(),
            Ok((Envelope::SavesRefused { game, reason }, _)) => {
                refused.push(json!({ "key": game.key, "title": game.title, "reason": reason }))
            }
            // A computer leaves no save sets for the handheld: its saves come at the next live sync.
            Ok((Envelope::Saves { .. }, _)) | Err(_) => {}
        }
        if let Some(name) = letter.path.file_name().and_then(|n| n.to_str()) {
            done.push(name.to_string());
        }
        let _ = std::fs::remove_file(&letter.path);
    }
    let peer_hex = peer.to_hex();
    let cursor = local.lib.cursors.get(&peer_hex).copied().unwrap_or_default();
    let (outgoing, seq) = local.lib.changes_since(cursor.pushed);
    let posted = outgoing.len();
    if !outgoing.is_empty() {
        let payload = mailbox::pack(&Envelope::Library { changes: outgoing }, &[])?;
        mailbox::post(&outbox, &key, &peer, &payload)?;
    }
    local.lib.cursors.entry(peer_hex).or_default().pushed = seq;
    local.save()?;
    Ok(json!({ "applied": applied, "posted": posted, "done": done, "refused": refused, "marks": local.to_write() }))
}

/// A game's saves left in the share for the computer, when they changed
/// since it last had them (`savesync::post_to_share`).
fn share_post_saves(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let peer = peer_of(args["peer"].as_str().unwrap_or_default())?;
    let outbox = path_arg(args, "outbox")?;
    let a: SavesArgs = serde_json::from_value(args.clone())?;
    let posted = savesync::post_to_share(&outbox, &key, &peer, &a.game, &a.roots(), &a.baseline)?;
    Ok(serde_json::to_value(posted)?)
}

// Plugin contexts ------------------------------------------------------------

fn sync_context(args: &Value) -> Outcome {
    let key = key_of(args)?;
    let decl: ContextDecl = serde_json::from_value(args["decl"].clone())?;
    let device: Records = serde_json::from_value(args["device"].clone()).unwrap_or_default();
    let baseline: Records = serde_json::from_value(args["baseline"].clone()).unwrap_or_default();
    let mut s = connect(&key, args)?;
    let ch = &mut s.ch;
    ch.send_json(&Request::ContextPull { context: decl.id.clone() })?;
    let theirs = match ch.recv_json::<Response>()? {
        Response::Context { records } => records,
        Response::Error { message } => return Err(Failure::Error(message)),
        other => return Err(Failure::Error(format!("the computer answered {other:?}"))),
    };
    let m = merge(&decl, &device, &theirs, &baseline);
    let (deferred, message) = if m.to_computer.is_empty() {
        (false, None)
    } else {
        ch.send_json(&Request::ContextPush { context: decl.id.clone(), changes: m.to_computer.clone() })?;
        match ch.recv_json::<Response>()? {
            Response::ContextApplied { deferred, message } => (deferred, message),
            Response::Error { message } => return Err(Failure::Error(message)),
            other => return Err(Failure::Error(format!("the computer answered {other:?}"))),
        }
    };
    let kept = if deferred { baseline_when_deferred(&m, &theirs) } else { m.baseline.clone() };
    let v = located(
        json!({
            "device": m.device,
            "baseline": kept,
            "conflicts": m.conflicts,
            "sent": m.to_computer.len(),
            "deferred": deferred,
            "message": message,
        }),
        &s,
    );
    bye(s.ch);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_round_trip_through_json() {
        let made: Value = serde_json::from_str(&call("identity_new", "{}")).unwrap();
        let again: Value = serde_json::from_str(&call("identity_id", &json!({ "seed": made["seed"] }).to_string())).unwrap();
        assert_eq!(made["id"], again["id"]);
        assert!(call("nope", "{}").contains("error"));
    }

    #[test]
    fn pairing_through_the_api_pins_both_sides() {
        let handheld: Value = serde_json::from_str(&call("identity_new", "{}")).unwrap();
        let started: Value = serde_json::from_str(&call(
            "pair_start",
            &json!({ "seed": handheld["seed"], "name": "Handheld", "address": "127.0.0.1" }).to_string(),
        ))
        .unwrap();
        let uri = started["uri"].as_str().unwrap().to_string();
        let computer = thread::spawn(move || {
            let invite = PairInvite::parse(&uri).unwrap();
            let mut s = TcpStream::connect(invite.at.unwrap()).unwrap();
            pairing::pair_client(&mut s, &DeviceKey::generate(), "PC", &invite.code).unwrap()
        });
        let waited: Value = serde_json::from_str(&call("pair_wait", r#"{"timeout_ms": 10000}"#)).unwrap();
        let paired = computer.join().unwrap();
        assert_eq!(waited["name"], "PC");
        assert_eq!(paired.name, "Handheld");
        assert_eq!(paired.peer.to_hex(), handheld["id"].as_str().unwrap());
    }

    #[test]
    fn pairing_with_the_code_the_computer_shows() {
        let handheld: Value = serde_json::from_str(&call("identity_new", "{}")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let pc = DeviceKey::generate();
        let pc_id = pc.peer_id();
        let computer = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            pairing::pair_host(&mut s, &pc, "PC", "246810").unwrap()
        });
        let out: Value = serde_json::from_str(&call(
            "pair_connect",
            &json!({ "seed": handheld["seed"], "name": "Handheld", "address": at.to_string(), "code": "246 810" }).to_string(),
        ))
        .unwrap();
        let paired = computer.join().unwrap();
        assert_eq!(out["peer"], json!(pc_id.to_hex()), "{out}");
        assert_eq!(out["name"], "PC");
        assert_eq!(out["address"], json!(format!("127.0.0.1:{PORT}")));
        assert_eq!(paired.name, "Handheld");
        assert_eq!(paired.peer.to_hex(), handheld["id"].as_str().unwrap());
    }

    /// A computer reachable only through its WireGuard endpoint, as one away
    /// from the LAN with a forwarded port or a global IPv6 address is.
    struct AwayHost {
        library: Mutex<Library>,
    }

    impl droidtop_agent_core::server::Host for AwayHost {
        fn name(&self) -> String {
            "Away PC".into()
        }
        fn endpoints(&self) -> Vec<String> {
            vec!["wg:203.0.113.7:47611".into()]
        }
        fn saves(&self, _game: &GameRef) -> Option<(droidtop_agent_core::saves::SaveSpec, Roots)> {
            None
        }
        fn archive_dir(&self, _game: &GameRef) -> PathBuf {
            std::env::temp_dir()
        }
        fn library_pull(
            &self,
            _peer: &PeerId,
            since: u64,
        ) -> droidtop_agent_core::Result<(Vec<droidtop_agent_core::library::Change>, u64)> {
            Ok(self.library.lock().unwrap().changes_since(since))
        }
        fn library_push(&self, _peer: &PeerId, changes: Vec<droidtop_agent_core::library::Change>) -> droidtop_agent_core::Result<()> {
            let mut lib = self.library.lock().unwrap();
            changes.into_iter().for_each(|c| {
                lib.apply(c);
            });
            Ok(())
        }
        fn context_pull(&self, _context: &str) -> droidtop_agent_core::Result<Records> {
            Ok(Records::new())
        }
        fn context_push(
            &self,
            _context: &str,
            _changes: Vec<droidtop_agent_core::context::RecordChange>,
        ) -> droidtop_agent_core::Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn a_computer_only_its_wireguard_endpoint_reaches_still_syncs() {
        let handheld: Value = serde_json::from_str(&call("identity_new", "{}")).unwrap();
        let hh_id = PeerId::from_hex(handheld["id"].as_str().unwrap()).unwrap();
        let pc = DeviceKey::generate();
        let pc_id = pc.peer_id();
        let mut pc_lib = Library::default();
        pc_lib.update_device(
            &pc_id,
            "Away PC",
            vec![ScannedGame { key: "steam:1".into(), title: "Game".into(), platform: None, install: Default::default() }],
        );
        pc_lib.mark(&pc_id, "steam:1", "favourite", json!(true));
        let host: &'static AwayHost = Box::leak(Box::new(AwayHost { library: Mutex::new(pc_lib) }));
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = udp.local_addr().unwrap().port();
        let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let pc_seed = pc.seed();
        let server = thread::spawn(move || {
            tunnel::serve(
                &pc,
                udp,
                move || vec![hh_id],
                move |peer, mut stream| {
                    thread::spawn(move || {
                        let key = DeviceKey::from_seed(&pc_seed).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(20)));
                        let mut ch = Channel::accept(stream, &key, |p| *p == peer).unwrap();
                        droidtop_agent_core::server::serve(&mut ch, host).unwrap();
                    });
                },
                stop,
            )
            .unwrap();
        });
        let state = std::env::temp_dir().join(format!("dta-away-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&state);
        let out: Value = serde_json::from_str(&call(
            "sync_library",
            &json!({
                "seed": handheld["seed"],
                "peer": pc_id.to_hex(),
                // No LAN address answers; only the endpoint the computer gave last time.
                "addresses": ["127.0.0.1:1", format!("wg:127.0.0.1:{port}")],
                "state": state,
                "scan": [{ "key": "steam:1", "title": "Game", "install": { "installed": true } }],
                "marks": { "steam:1": { "favourite": false, "hidden": false } },
            })
            .to_string(),
        ))
        .unwrap();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let _ = std::fs::remove_file(&state);
        assert!(out.get("unreachable").is_none() && out.get("error").is_none(), "{out}");
        assert_eq!(out["computer"], "Away PC");
        assert!(out.get("address").is_none(), "a tunnel is not a LAN address to remember: {out}");
        assert_eq!(out["endpoints"], json!(["wg:203.0.113.7:47611"]));
        assert_eq!(out["pulled"], 2);
        // The computer's favourite is for droidtop to write; nothing was sent back.
        assert_eq!(out["marks"], json!({ "steam:1": { "favourite": true } }));
        assert_eq!(host.library.lock().unwrap().games["steam:1"].installs.len(), 2);
    }

    #[test]
    fn an_unreachable_computer_says_so() {
        let handheld: Value = serde_json::from_str(&call("identity_new", "{}")).unwrap();
        let pc = DeviceKey::generate().peer_id().to_hex();
        let out: Value = serde_json::from_str(&call(
            "sync_library",
            &json!({ "seed": handheld["seed"], "peer": pc, "addresses": ["127.0.0.1:1"], "state": std::env::temp_dir().join("dta-unreachable.json"), "scan": [] }).to_string(),
        ))
        .unwrap();
        assert!(out.get("unreachable").is_some(), "{out}");
    }
}
