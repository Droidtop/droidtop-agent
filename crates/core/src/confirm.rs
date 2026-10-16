//! Pairing by comparing a code (docs/DESIGN.md section 3; droidtop SPEC 7o,
//! "Pairing with Pythia"): both screens show one 6-digit code and the person
//! confirms on both that they match, so nothing is typed. A numeric
//! comparison with a commitment, as Bluetooth's and ZRTP's are:
//!
//! 1. the side that picked the other (the client) sends its id and a
//!    commitment, SHA-256 over the label, its id and a fresh 32-byte value;
//! 2. the other side (the host) replies with its id and its own fresh value;
//! 3. the client reveals its value and the host checks the commitment;
//! 4. both derive the code, HKDF-SHA256 over both values (salt: the label;
//!    info: both ids), as six digits;
//! 5. after the person's answer on each side, both send it; when both said
//!    the codes match, each describes itself (name, kind, certificate
//!    fingerprint) signed by its identity over the transcript, as the typed
//!    exchange does (`crate::pairing`), and pins the other's certificate.
//!
//! The commitment fixes the client's value before it sees the host's, so a
//! device in the middle cannot choose values that make its two codes agree:
//! it has one chance in a million per attempt.

use std::io::{Read, Write};

use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windowcast_identity::Identity;
use windowcast_pairing::exchange::{self, Proof};

use crate::keys::{DeviceKey, PeerId};
use crate::pairing::{clip, recv, send, About, Description, Kind, Paired};
use crate::{cert, hex, Error, Result};

/// This exchange's label in every commitment, code and transcript.
const LABEL: &[u8] = b"droidtop-agent pair-confirm v1\0";

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Commit {
    v: u32,
    id: String,
    commit: String,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    v: u32,
    id: String,
    value: String,
}

#[derive(Serialize, Deserialize)]
struct Reveal {
    value: String,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    same: bool,
}

fn commitment(peer: &PeerId, value: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(LABEL);
    h.update(peer.0);
    h.update(value);
    h.finalize().into()
}

/// The six digits both screens show.
pub fn code(client: &PeerId, host: &PeerId, client_value: &[u8; 32], host_value: &[u8; 32]) -> String {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(client_value);
    ikm[32..].copy_from_slice(host_value);
    let mut info = [0u8; 64];
    info[..32].copy_from_slice(&client.0);
    info[32..].copy_from_slice(&host.0);
    let mut out = [0u8; 8];
    Hkdf::<Sha256>::new(Some(LABEL), &ikm).expand(&info, &mut out).expect("8 bytes is a valid HKDF length");
    format!("{:06}", u64::from_be_bytes(out) % 1_000_000)
}

fn fresh() -> [u8; 32] {
    let mut v = [0u8; 32];
    OsRng.fill_bytes(&mut v);
    v
}

fn peer_of(id: &str) -> Result<PeerId> {
    let peer = PeerId::from_hex(id).map_err(|_| Error::Pairing("a malformed device id".into()))?;
    crate::keys::x25519_public_of(&peer)?;
    Ok(peer)
}

fn value_of(text: &str) -> Result<[u8; 32]> {
    hex::decode(text).and_then(|b| b.try_into().ok()).ok_or_else(|| Error::Pairing("a malformed pairing message".into()))
}

fn check_version(v: u32) -> Result<()> {
    if v != VERSION {
        return Err(Error::Pairing("the other device runs an older droidtop-agent or droidtop; update both and pair again".into()));
    }
    Ok(())
}

/// An exchange waiting for the person: [`code`](Comparing::code) is what this
/// screen shows, [`other`](Comparing::other) the device on the other end.
pub struct Comparing {
    /// 0 on the client (the side that picked), 1 on the host.
    side: u8,
    client: exchange::Hello,
    host: exchange::Hello,
    pub code: String,
    pub other: PeerId,
}

fn hello(peer: PeerId, nonce: [u8; 32]) -> exchange::Hello {
    exchange::Hello { peer, nonce, mode: 0 }
}

/// The side that picked the other from a list; it speaks first.
pub fn client_start<S: Read + Write>(stream: &mut S, key: &DeviceKey) -> Result<Comparing> {
    let me = key.peer_id();
    let mine = fresh();
    send(stream, &Commit { v: VERSION, id: me.to_hex(), commit: hex::encode(&commitment(&me, &mine)) })?;
    let reply: Reply = recv(stream)?;
    check_version(reply.v)?;
    let host = peer_of(&reply.id)?;
    let theirs = value_of(&reply.value)?;
    send(stream, &Reveal { value: hex::encode(&mine) })?;
    Ok(Comparing { side: 0, code: code(&me, &host, &mine, &theirs), other: host, client: hello(me, mine), host: hello(host, theirs) })
}

/// The side that was picked; it answers.
pub fn host_start<S: Read + Write>(stream: &mut S, key: &DeviceKey) -> Result<Comparing> {
    let me = key.peer_id();
    let first: Commit = recv(stream)?;
    check_version(first.v)?;
    let client = peer_of(&first.id)?;
    let committed = value_of(&first.commit)?;
    let mine = fresh();
    send(stream, &Reply { v: VERSION, id: me.to_hex(), value: hex::encode(&mine) })?;
    let reveal: Reveal = recv(stream)?;
    let theirs = value_of(&reveal.value)?;
    if commitment(&client, &theirs) != committed {
        return Err(Error::Pairing("the other device changed its value after committing to it; nothing was paired".into()));
    }
    Ok(Comparing { side: 1, code: code(&client, &me, &theirs, &mine), other: client, client: hello(client, theirs), host: hello(me, mine) })
}

impl Comparing {
    /// Sends the person's answer, reads the other side's, and when both said
    /// the codes match, exchanges descriptions and returns the device to pin.
    pub fn finish<S: Read + Write>(self, stream: &mut S, key: &DeviceKey, name: &str, kind: Kind, same: bool) -> Result<Paired> {
        send(stream, &Answer { same })?;
        let theirs: Answer = recv(stream)?;
        if !same {
            return Err(Error::Pairing("the codes did not match; nothing was paired".into()));
        }
        if !theirs.same {
            return Err(Error::Pairing("the other device said the codes did not match; nothing was paired".into()));
        }
        if self.side == 0 {
            self.describe(stream, key, name, kind)?;
            self.described(stream)
        } else {
            let paired = self.described(stream)?;
            self.describe(stream, key, name, kind)?;
            Ok(paired)
        }
    }

    fn describe(&self, stream: &mut impl Write, key: &DeviceKey, name: &str, kind: Kind) -> Result<()> {
        let about = serde_json::to_string(&About { name: clip(name), kind, cert: cert::fingerprint_hex(&key.peer_id()) })?;
        let t = exchange::transcript(LABEL, self.side, &self.client, &self.host, &[0; 32], about.as_bytes());
        let proof = exchange::prove(&Identity::from_seed(&key.seed()), None, &t);
        send(stream, &Description { about, signature: hex::encode(&proof.signature), tag: String::new() })
    }

    fn described(&self, stream: &mut impl Read) -> Result<Paired> {
        let d: Description = recv(stream)?;
        let side = 1 - self.side;
        let t = exchange::transcript(LABEL, side, &self.client, &self.host, &[0; 32], d.about.as_bytes());
        let proof = Proof { signature: hex::decode(&d.signature).unwrap_or_default(), pin_tag: None };
        exchange::check(&self.other, None, &t, &proof)
            .map_err(|_| Error::Pairing("the other device's description was not signed by its key".into()))?;
        let about: About = serde_json::from_str(&d.about).map_err(|_| Error::Pairing("a malformed description".into()))?;
        let fingerprint = cert::fingerprint_hex(&self.other);
        if about.cert != fingerprint {
            return Err(Error::Pairing("the other device's certificate is not the one of its key; nothing was paired".into()));
        }
        Ok(Paired { peer: self.other, name: clip(&about.name), kind: about.kind, fingerprint })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn pair(host_same: bool, client_same: bool) -> (Result<Paired>, Result<Paired>, String, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let host_key = DeviceKey::generate();
        let host = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let c = host_start(&mut s, &host_key).unwrap();
            let code = c.code.clone();
            (c.finish(&mut s, &host_key, "Desk", Kind::Computer, host_same), code)
        });
        let client_key = DeviceKey::generate();
        let mut s = TcpStream::connect(addr).unwrap();
        let c = client_start(&mut s, &client_key).unwrap();
        let code = c.code.clone();
        let client = c.finish(&mut s, &client_key, "Pocket", Kind::Handheld, client_same);
        let (host, host_code) = host.join().unwrap();
        (host, client, host_code, code)
    }

    #[test]
    fn both_screens_show_one_code_and_both_pin() {
        let (host, client, host_code, client_code) = pair(true, true);
        assert_eq!(host_code, client_code);
        assert_eq!(host_code.len(), 6);
        let (host, client) = (host.unwrap(), client.unwrap());
        assert_eq!((host.name.as_str(), host.kind), ("Pocket", Kind::Handheld));
        assert_eq!((client.name.as_str(), client.kind), ("Desk", Kind::Computer));
        assert_eq!(client.fingerprint, cert::fingerprint_hex(&client.peer));
    }

    #[test]
    fn a_no_on_either_side_pairs_nothing() {
        let (host, client, ..) = pair(true, false);
        assert!(host.is_err() && client.is_err());
        let (host, client, ..) = pair(false, true);
        assert!(host.is_err() && client.is_err());
    }

    #[test]
    fn a_value_changed_after_the_commitment_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let host = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            host_start(&mut s, &DeviceKey::generate()).map(|_| ())
        });
        let key = DeviceKey::generate();
        let mut s = TcpStream::connect(addr).unwrap();
        send(&mut s, &Commit { v: VERSION, id: key.peer_id().to_hex(), commit: hex::encode(&commitment(&key.peer_id(), &fresh())) })
            .unwrap();
        let _: Reply = recv(&mut s).unwrap();
        send(&mut s, &Reveal { value: hex::encode(&fresh()) }).unwrap();
        assert!(host.join().unwrap().is_err());
    }

    #[test]
    fn the_code_depends_on_every_input() {
        let (a, b) = (DeviceKey::generate().peer_id(), DeviceKey::generate().peer_id());
        let (x, y) = ([1u8; 32], [2u8; 32]);
        let base = code(&a, &b, &x, &y);
        assert_ne!(base, code(&b, &a, &x, &y));
        assert_ne!(base, code(&a, &b, &y, &x));
        assert_ne!(base, code(&a, &b, &x, &[3u8; 32]));
        assert_eq!(base, code(&a, &b, &x, &y));
    }
}
