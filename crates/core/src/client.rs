//! Reaching a paired device as the side that asks (docs/DESIGN.md section
//! 10): its known LAN addresses, then the LAN by broadcast, then the
//! WireGuard endpoints it stated, then a rendezvous through global
//! discovery. droidtop's library uses it to reach a computer, and a computer
//! uses it to reach a device that offers it something (section 20). Every
//! path carries the same mutual-TLS session channel, which answers only the
//! pinned peer.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::channel::Channel;
use crate::discovery;
use crate::keys::{DeviceKey, PeerId};
use crate::proto::{Request, Response};
use crate::tunnel::{self, TunnelStream};
use crate::{rendezvous, Error, FEATURES, PORT, PROTOCOL_VERSION};

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

/// Which way a session reached the other device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// This network: a known address or the LAN broadcast.
    Lan,
    /// WireGuard to an endpoint the other device stated (a forwarded port, IPv6).
    Wireguard,
    /// WireGuard through a hole punched after a global discovery rendezvous.
    Rendezvous,
}

impl Path {
    pub fn name(self) -> &'static str {
        match self {
            Path::Lan => "lan",
            Path::Wireguard => "wireguard",
            Path::Rendezvous => "rendezvous",
        }
    }
}

/// A session with the other device: where and how it answered, its name,
/// its WireGuard endpoints and its global discovery ID.
pub struct Session {
    pub ch: Channel<Link>,
    /// Where it answered on this network; empty through a tunnel.
    pub address: String,
    pub path: Path,
    /// Its name, as its hello said.
    pub name: String,
    pub endpoints: Vec<String>,
    pub disco: Option<String>,
    /// What it supports, and offers this device (the `features` of its
    /// hello); an older agent states fewer.
    pub features: Vec<String>,
    /// It moved to a new identity, signed by both keys; the caller re-pins it.
    pub moved_to: Option<PeerId>,
}

impl Session {
    /// Whether its hello stated [`feature`].
    pub fn states(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// Ends the session politely.
    pub fn bye(mut self) {
        let _ = self.ch.send_json(&Request::Bye);
    }
}

/// Why a device could not be reached: it did not answer by any path, or it
/// answered and something failed.
#[derive(Debug)]
pub enum ConnectError {
    Unreachable(String),
    Failed(Error),
}

impl From<Error> for ConnectError {
    fn from(e: Error) -> Self {
        ConnectError::Failed(e)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unreachable(m) => f.write_str(m),
            ConnectError::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// The rendezvous settings: the discovery servers (`default` is
/// Syncthing's), the STUN servers, and a small file where this device keeps
/// when it may announce and ask again.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RendezvousSettings {
    #[serde(default = "default_list")]
    pub servers: Vec<String>,
    #[serde(default = "default_list")]
    pub stun: Vec<String>,
    pub state: PathBuf,
}

fn default_list() -> Vec<String> {
    vec!["default".into()]
}

/// The device to reach and what is known of where it is.
pub struct Target<'a> {
    pub peer: PeerId,
    /// This device's name, said in the hello.
    pub name: &'a str,
    /// Addresses it was last seen at (`host:port`, or `host` for the
    /// agent's port), and its WireGuard endpoints (`wg:<ip>:<port>`).
    pub addresses: &'a [String],
    /// Its global discovery ID, for a rendezvous.
    pub disco: Option<&'a str>,
    pub rendezvous: Option<&'a RendezvousSettings>,
}

fn parse_address(text: &str) -> Option<SocketAddr> {
    text.parse().ok().or_else(|| format!("{text}:{PORT}").parse().ok())
}

fn hello(mut ch: Channel<Link>, address: String, path: Path, key: &DeviceKey, name: &str) -> Result<Session, ConnectError> {
    let features = FEATURES.iter().map(|f| f.to_string()).collect();
    let disco = rendezvous::certificate(key).ok().map(|c| c.device_id());
    ch.send_json(&Request::Hello { name: name.to_string(), version: PROTOCOL_VERSION, features, disco })?;
    let (name, endpoints, disco, moved, features) = match ch.recv_json::<Response>()? {
        Response::Hello { name, endpoints, disco, moved_to, features, .. } => (name, endpoints, disco, moved_to, features),
        _ => (String::new(), Vec::new(), None, None, Vec::new()),
    };
    // Only a move both keys signed, from the identity this session reached.
    let moved_to = moved.and_then(|m| m.check(&ch.peer()));
    Ok(Session { ch, address, path, name, endpoints, disco, features, moved_to })
}

/// A session with [`target`], in the design's order.
pub fn connect(key: &DeviceKey, target: &Target) -> Result<Session, ConnectError> {
    let peer = target.peer;
    let mut candidates: Vec<SocketAddr> =
        target.addresses.iter().filter(|a| !a.starts_with("wg:")).filter_map(|a| parse_address(a)).collect();
    let tunnels: Vec<SocketAddr> = target.addresses.iter().filter_map(|a| a.strip_prefix("wg:")).filter_map(|a| a.parse().ok()).collect();
    let mut tried_lan = false;
    let mut last = String::from("it did not answer on this network");
    loop {
        for addr in std::mem::take(&mut candidates) {
            let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(3)) else { continue };
            if let Err(e) = stream
                .set_read_timeout(Some(Duration::from_secs(120)))
                .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(120))))
            {
                last = format!("its connection could not be set up ({e})");
                continue;
            }
            match Channel::connect(Link::Tcp(stream), key, &peer) {
                Ok(ch) => return hello(ch, addr.to_string(), Path::Lan, key, target.name),
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
                return hello(ch, String::new(), Path::Wireguard, key, target.name);
            }
            Err(e) => last = format!("{last}; through WireGuard: {e}"),
        }
    }
    // Last, a rendezvous through global discovery, when it is on and the
    // device's discovery ID is known from an earlier session.
    if let (Some(settings), Some(disco)) = (target.rendezvous, target.disco.filter(|d| !d.is_empty())) {
        match rendezvous_tunnel(key, &peer, disco, settings) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(120)));
                let ch = Channel::connect(Link::Tunnel(stream), key, &peer)?;
                return hello(ch, String::new(), Path::Rendezvous, key, target.name);
            }
            Err(e) => last = format!("{last}; through global discovery: {e}"),
        }
    }
    Err(ConnectError::Unreachable(last))
}

/// When this device last announced, what, and when it may ask again, so its
/// use of the discovery servers stays at Syncthing's own client's pace.
#[derive(Serialize, Deserialize, Default)]
struct RendezvousState {
    #[serde(default)]
    announced: Option<String>,
    #[serde(default)]
    announce_after_ms: i64,
    /// By discovery ID: no lookup before this time.
    #[serde(default)]
    lookup_after_ms: std::collections::BTreeMap<String, i64>,
    /// By discovery ID: what the last lookup found, and until when it is used.
    #[serde(default)]
    found: std::collections::BTreeMap<String, (Vec<String>, i64)>,
}

fn later_ms(d: Duration) -> i64 {
    crate::library::now_ms() + d.as_millis() as i64
}

/// The device away from both LAN and stated endpoints: ask global discovery
/// where it is, tell it where this device is, and punch through both NATs
/// with WireGuard handshakes from the socket whose mapping was announced. It
/// answers only its pinned keys, and every byte goes through that tunnel.
fn rendezvous_tunnel(key: &DeviceKey, peer: &PeerId, disco: &str, settings: &RendezvousSettings) -> Result<TunnelStream, String> {
    let mut state: RendezvousState = std::fs::read(&settings.state).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let now = crate::library::now_ms();
    let cached = state.found.get(disco).filter(|(_, until)| *until > now).map(|(a, _)| a.clone());
    if cached.is_none() && state.lookup_after_ms.get(disco).is_some_and(|t| *t > now) {
        return Err("global discovery did not know where it was a moment ago; it is asked again in a minute".into());
    }
    let servers = rendezvous::Server::list(&settings.servers);
    let udp = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let stun = rendezvous::resolve_stun(&settings.stun);
    let mine = rendezvous::query_stun(&udp, &stun, Duration::from_secs(2));
    let answer = match cached {
        Some(addresses) => rendezvous::Answer::Found(addresses),
        None => rendezvous::lookup(&servers, disco),
    };
    let found = match answer {
        rendezvous::Answer::Found(addresses) => {
            // Syncthing keeps a found address for five minutes.
            if !state.found.get(disco).is_some_and(|(_, until)| *until > now) {
                state.found.insert(disco.to_string(), (addresses.clone(), later_ms(rendezvous::FOUND_CACHE)));
            }
            addresses
        }
        rendezvous::Answer::Wait(d) | rendezvous::Answer::Announced(d) => {
            state.lookup_after_ms.insert(disco.to_string(), later_ms(d));
            let _ = std::fs::write(&settings.state, serde_json::to_vec(&state).unwrap_or_default());
            return Err("global discovery does not know where it is (it announces itself while droidtop-agent runs)".into());
        }
    };
    // Tell the other device where to punch from: the address the NAT gave
    // this socket, announced again only when it changed or the server asked.
    if let (Some(mine), Ok(cert)) = (mine, rendezvous::certificate(key)) {
        let address = format!("{}{mine}", rendezvous::SCHEME);
        if state.announced.as_deref() != Some(address.as_str()) || state.announce_after_ms <= now {
            match rendezvous::announce(&servers, &cert, std::slice::from_ref(&address)) {
                rendezvous::Answer::Announced(d) => {
                    state.announced = Some(address);
                    state.announce_after_ms = later_ms(d);
                }
                rendezvous::Answer::Wait(d) => state.announce_after_ms = later_ms(d),
                rendezvous::Answer::Found(_) => {}
            }
        }
    }
    let _ = std::fs::write(&settings.state, serde_json::to_vec(&state).unwrap_or_default());
    let targets = rendezvous::wg_endpoints(&found);
    if targets.is_empty() {
        return Err("global discovery knows the device but no WireGuard address for it".into());
    }
    // The other device looks this one up on Syncthing's schedule (about once
    // a minute while it has not found it), so the handshakes keep coming that long.
    tunnel::connect_on(udp, key, peer, &targets, Duration::from_secs(75)).map_err(|e| e.to_string())
}
