//! Store and forward through the person's own cloud share (docs/DESIGN.md
//! section 10, transport 3). A share is a folder the person's own sync tool
//! carries to both devices. Each device leaves sealed messages for a peer in
//! `droidtop-agent/<peer id>/inbox/`; the peer opens, applies and removes
//! them when they arrive. The share only ever holds ciphertext.
//!
//! Sealing: XChaCha20-Poly1305 under a key derived from X25519 between the
//! two identities, a random 24-byte nonce per message, and the sender, the
//! recipient and the format in the associated data.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use serde::{Deserialize, Serialize};

use crate::keys::{short, DeviceKey, PeerId};
use crate::library::Change;
use crate::manifest::FileEntry;
use crate::proto::GameRef;
use crate::{error::protocol, Error, Result};

const MAGIC: &[u8; 4] = b"DTM1";
const EXT: &str = "dtmsg";

fn box_key(key: &DeviceKey, peer: &PeerId) -> Result<[u8; 32]> {
    let shared = key.shared_secret(peer)?;
    let mut h = Sha256::new();
    h.update(b"droidtop-agent mailbox v1");
    h.update(shared);
    Ok(h.finalize().into())
}

fn aad(from: &PeerId, to: &PeerId) -> Vec<u8> {
    let mut a = MAGIC.to_vec();
    a.extend_from_slice(&from.0);
    a.extend_from_slice(&to.0);
    a
}

/// [`payload`] sealed from [`key`]'s device to [`to`].
pub fn seal(key: &DeviceKey, to: &PeerId, payload: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(&box_key(key, to)?.into());
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let from = key.peer_id();
    let sealed = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: payload, aad: &aad(&from, to) })
        .map_err(|_| protocol("a message could not be sealed"))?;
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&from.0);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// The sender and payload of a message sealed for [`key`]'s device, when the
/// sender is one [`trusted`] accepts.
pub fn open(key: &DeviceKey, bytes: &[u8], trusted: impl Fn(&PeerId) -> bool) -> Result<(PeerId, Vec<u8>)> {
    if bytes.len() < 4 + 32 + 24 + 16 || &bytes[..4] != MAGIC {
        return Err(protocol("not a droidtop-agent message"));
    }
    let from = PeerId(bytes[4..36].try_into().expect("32 bytes"));
    if !trusted(&from) {
        return Err(Error::NotPaired);
    }
    let cipher = XChaCha20Poly1305::new(&box_key(key, &from)?.into());
    let payload = cipher
        .decrypt(XNonce::from_slice(&bytes[36..60]), Payload { msg: &bytes[60..], aad: &aad(&from, &key.peer_id()) })
        .map_err(|_| protocol("a message was damaged or not for this device"))?;
    Ok((from, payload))
}

/// The folder messages for [`peer`] go to under [`share`].
pub fn inbox(share: &Path, peer: &PeerId) -> PathBuf {
    share.join("droidtop-agent").join(peer.to_hex()).join("inbox")
}

/// Leaves a sealed message for [`to`] in [`share`]; written under a
/// temporary name and renamed, so a sync tool never carries half of it.
pub fn post(share: &Path, key: &DeviceKey, to: &PeerId, payload: &[u8]) -> Result<PathBuf> {
    let dir = inbox(share, to);
    fs::create_dir_all(&dir)?;
    let mut tag = [0u8; 6];
    OsRng.fill_bytes(&mut tag);
    let name = format!("{}-{}-{}.{EXT}", short(&key.peer_id()), crate::library::now_ms(), crate::hex::encode(&tag));
    let tmp = dir.join(format!(".{name}.part"));
    fs::write(&tmp, seal(key, to, payload)?)?;
    let path = dir.join(name);
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// What a message carries. File contents follow the header in [`pack`]'s
/// layout, in the order the header lists them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Envelope {
    /// Library changes the recipient has not had yet.
    Library { changes: Vec<Change> },
    /// A game's whole save set, made when no live path was there, with the
    /// baseline the sender last agreed on with the recipient. When the
    /// recipient's saves changed too, the newest copy wins as in a live sync
    /// (a preferred side first: [`primary_computer`] says the sender prefers
    /// the recipient's), and the other set is kept as its device's copy.
    Saves {
        game: GameRef,
        base: Vec<FileEntry>,
        files: Vec<FileEntry>,
        #[serde(default)]
        primary_computer: bool,
    },
    /// The recipient could not apply a save set, and why.
    SavesRefused { game: GameRef, reason: String },
}

/// An envelope and its file contents as one payload: a 4-byte header length,
/// the JSON header, then the contents back to back.
pub fn pack(envelope: &Envelope, contents: &[Vec<u8>]) -> Result<Vec<u8>> {
    let header = serde_json::to_vec(envelope)?;
    let mut out = (header.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&header);
    for c in contents {
        out.extend_from_slice(c);
    }
    Ok(out)
}

/// The envelope and the bytes after its header.
pub fn unpack(payload: &[u8]) -> Result<(Envelope, &[u8])> {
    if payload.len() < 4 {
        return Err(protocol("an empty message"));
    }
    let len = u32::from_be_bytes(payload[..4].try_into().expect("4 bytes")) as usize;
    let header = payload.get(4..4 + len).ok_or_else(|| protocol("a truncated message"))?;
    Ok((serde_json::from_slice(header)?, &payload[4 + len..]))
}

/// The letters opened, and the files left unopened with the reason.
pub type Collected = (Vec<Letter>, Vec<(PathBuf, String)>);

/// One message waiting in this device's inbox.
pub struct Letter {
    pub from: PeerId,
    pub payload: Vec<u8>,
    pub path: PathBuf,
}

/// The messages waiting for [`key`]'s device in [`share`], oldest first.
/// Messages it cannot open are left where they are (a sync tool may still
/// be writing them) and reported in the second list.
pub fn collect(share: &Path, key: &DeviceKey, trusted: impl Fn(&PeerId) -> bool) -> io::Result<Collected> {
    let dir = inbox(share, &key.peer_id());
    let mut names: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(read) => read.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == EXT)).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(e),
    };
    names.sort();
    let mut letters = Vec::new();
    let mut failed = Vec::new();
    for path in names {
        let bytes = fs::read(&path)?;
        match open(key, &bytes, &trusted) {
            Ok((from, payload)) => letters.push(Letter { from, payload, path }),
            Err(e) => failed.push((path, e.to_string())),
        }
    }
    Ok((letters, failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_messages_open_only_for_their_recipient() {
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        let c = DeviceKey::generate();
        let sealed = seal(&a, &b.peer_id(), b"saves").unwrap();
        let (from, payload) = open(&b, &sealed, |_| true).unwrap();
        assert_eq!(from, a.peer_id());
        assert_eq!(payload, b"saves");
        assert!(open(&c, &sealed, |_| true).is_err());
        assert!(matches!(open(&b, &sealed, |_| false), Err(Error::NotPaired)));
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(&b, &tampered, |_| true).is_err());
    }

    #[test]
    fn posted_letters_are_collected() {
        let share = std::env::temp_dir().join(format!("dta-share-{}", std::process::id()));
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        post(&share, &a, &b.peer_id(), b"one").unwrap();
        post(&share, &a, &b.peer_id(), b"two").unwrap();
        let (letters, failed) = collect(&share, &b, |p| *p == a.peer_id()).unwrap();
        assert!(failed.is_empty());
        let mut got: Vec<Vec<u8>> = letters.into_iter().map(|l| l.payload).collect();
        got.sort();
        assert_eq!(got, vec![b"one".to_vec(), b"two".to_vec()]);
        let _ = fs::remove_dir_all(&share);
    }

    #[test]
    fn envelopes_pack_with_their_contents() {
        let e = Envelope::SavesRefused { game: GameRef { key: "steam:1".into(), title: "G".into() }, reason: "busy".into() };
        let packed = pack(&e, &[b"abc".to_vec()]).unwrap();
        let (back, rest) = unpack(&packed).unwrap();
        assert_eq!(back, e);
        assert_eq!(rest, b"abc");
    }
}
