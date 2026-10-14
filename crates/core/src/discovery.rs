//! Finding each other on the LAN (docs/DESIGN.md sections 3 and 10): small
//! UDP broadcasts on the agent's port.
//!
//! - The handheld asks which of its paired computers are there; an agent
//!   answers only a device it has pinned, signs the answer with its key, and
//!   the handheld checks the signature, so a stranger cannot pose as the
//!   computer (and the Noise handshake would refuse it anyway).
//! - While the pairing screen is open, the handheld answers the computer's
//!   pairing query with its id, name and port. Nothing there is trusted:
//!   SPAKE2 is what authenticates the pairing.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::hex;
use crate::keys::{verify, DeviceKey, PeerId};

const QUERY: &[u8] = b"DTAQ1";
const ANSWER: &[u8] = b"DTAR1";
const PAIR_QUERY: &[u8] = b"DTPQ1";
const PAIR_ANSWER: &[u8] = b"DTPR1";

#[derive(Serialize, Deserialize, Debug)]
struct Query {
    nonce: String,
    #[serde(default)]
    from: String,
}

/// A device's answer: who it is and the TCP port it listens on.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Announce {
    pub id: String,
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub sig: String,
}

/// A device found on the LAN, at its TCP address.
#[derive(Debug, Clone)]
pub struct Found {
    pub peer: PeerId,
    pub name: String,
    pub addr: SocketAddr,
}

fn nonce() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(&bytes)
}

fn signed(nonce: &str, a: &Announce) -> Vec<u8> {
    format!("droidtop-agent announce v1\n{nonce}\n{}\n{}\n{}", a.id, a.name, a.port).into_bytes()
}

fn packet(magic: &[u8], body: &impl Serialize) -> Vec<u8> {
    let mut p = magic.to_vec();
    p.extend_from_slice(&serde_json::to_vec(body).unwrap_or_default());
    p
}

fn body<'a>(magic: &[u8], packet: &'a [u8]) -> Option<&'a [u8]> {
    packet.strip_prefix(magic)
}

fn broadcast_socket() -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_broadcast(true)?;
    Ok(sock)
}

/// Receives until [`deadline`], handing each packet to [`each`].
fn gather(sock: &UdpSocket, timeout: Duration, mut each: impl FnMut(&[u8], SocketAddr)) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        sock.set_read_timeout(Some(left))?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => each(&buf[..n], from),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// The handheld: which of [`wanted`] answer on the LAN within [`timeout`].
pub fn find_agents(me: &PeerId, wanted: &[PeerId], timeout: Duration) -> io::Result<Vec<Found>> {
    let sock = broadcast_socket()?;
    let nonce = nonce();
    sock.send_to(&packet(QUERY, &Query { nonce: nonce.clone(), from: me.to_hex() }), (Ipv4Addr::BROADCAST, crate::PORT))?;
    let mut found: Vec<Found> = Vec::new();
    gather(&sock, timeout, |data, from| {
        let Some(a) = body(ANSWER, data).and_then(|b| serde_json::from_slice::<Announce>(b).ok()) else { return };
        let Ok(peer) = PeerId::from_hex(&a.id) else { return };
        let sig = hex::decode(&a.sig).unwrap_or_default();
        if !wanted.contains(&peer) || !verify(&peer, &signed(&nonce, &a), &sig) || found.iter().any(|f| f.peer == peer) {
            return;
        }
        found.push(Found { peer, name: a.name, addr: SocketAddr::new(from.ip(), a.port) });
    })?;
    Ok(found)
}

/// The computer: answers the handheld's queries until [`stop`], on a socket
/// bound to the agent's port. Only devices [`trusted`] says are paired get
/// an answer.
pub fn answer_agent_queries(
    sock: &UdpSocket,
    key: &DeviceKey,
    name: &str,
    port: u16,
    trusted: impl Fn(&PeerId) -> bool,
    stop: &AtomicBool,
) -> io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(x) => x,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        let Some(q) = body(QUERY, &buf[..n]).and_then(|b| serde_json::from_slice::<Query>(b).ok()) else { continue };
        let Ok(asker) = PeerId::from_hex(&q.from) else { continue };
        if !trusted(&asker) {
            continue;
        }
        let mut a = Announce { id: key.peer_id().to_hex(), name: name.to_string(), port, sig: String::new() };
        a.sig = hex::encode(&key.sign(&signed(&q.nonce, &a)));
        let _ = sock.send_to(&packet(ANSWER, &a), from);
    }
    Ok(())
}

/// The computer, pairing: handhelds showing a pairing code on the LAN.
pub fn find_pairing(timeout: Duration) -> io::Result<Vec<(Announce, SocketAddr)>> {
    let sock = broadcast_socket()?;
    sock.send_to(&packet(PAIR_QUERY, &Query { nonce: nonce(), from: String::new() }), (Ipv4Addr::BROADCAST, crate::PORT))?;
    let mut found: Vec<(Announce, SocketAddr)> = Vec::new();
    gather(&sock, timeout, |data, from| {
        let Some(a) = body(PAIR_ANSWER, data).and_then(|b| serde_json::from_slice::<Announce>(b).ok()) else { return };
        if PeerId::from_hex(&a.id).is_err() || found.iter().any(|(f, _)| f.id == a.id) {
            return;
        }
        let addr = SocketAddr::new(from.ip(), a.port);
        found.push((a, addr));
    })?;
    Ok(found)
}

/// The handheld, pairing: answers pairing queries until [`stop`].
pub fn answer_pairing(sock: &UdpSocket, me: &Announce, stop: &AtomicBool) -> io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(x) => x,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        if body(PAIR_QUERY, &buf[..n]).is_some() {
            let _ = sock.send_to(&packet(PAIR_ANSWER, me), from);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_signed_over_the_nonce() {
        let key = DeviceKey::generate();
        let mut a = Announce { id: key.peer_id().to_hex(), name: "PC".into(), port: 47610, sig: String::new() };
        let sig = key.sign(&signed("n1", &a));
        a.sig = hex::encode(&sig);
        assert!(verify(&key.peer_id(), &signed("n1", &a), &sig));
        assert!(!verify(&key.peer_id(), &signed("n2", &a), &sig));
    }
}
