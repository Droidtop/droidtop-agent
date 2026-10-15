//! Pairing two devices (docs/DESIGN.md section 3): a computer with a
//! handheld, or two handhelds. windowcast's pairing exchange
//! (`windowcast_pairing::exchange`), the one windowcast's own signaling
//! runs, over a plain TCP stream. Both sides send a hello (identity and a
//! fresh nonce) with their SPAKE2 message, then each describes itself (its
//! name, its kind, and its certificate's fingerprint, [`crate::cert`]) with
//! a proof over the transcript: its identity's signature and the tag of the
//! key the code derived. Each checks the fingerprint is the one of the key
//! it proved, then pins it: every later session is mutual TLS against it.
//!
//! The side that shows the 6-digit code is the SPAKE2 host and listens; the
//! side the code is typed on is the client, connects and speaks first. The
//! handheld shows it by default; the computer shows it (`droidtop-agent pair`)
//! for a handheld it cannot reach, such as one behind an emulator's NAT.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use windowcast_identity::Identity;
use windowcast_pairing::exchange::{self, Proof};
use windowcast_pairing::{finish, start_client, start_host, SessionKey};

use crate::frame::{read_frame, write_frame, MAX_PLAIN_FRAME};
use crate::keys::{DeviceKey, PeerId};
use crate::{cert, hex, Error, Result};

/// The URI scheme of the QR code the handheld shows.
pub const SCHEME: &str = "droidtop-pair:1";

/// Device names are shown, never trusted; longer ones are cut.
pub const MAX_NAME: usize = 64;

/// What the person gives the computer to pair: the code, and when it came
/// from the QR code, the handheld's id, name and address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairInvite {
    pub code: String,
    pub peer: Option<PeerId>,
    pub name: Option<String>,
    pub at: Option<String>,
}

impl PairInvite {
    /// `droidtop-pair:1?code=123456&id=<hex>&name=<percent-encoded>&at=<ip:port>`.
    pub fn to_uri(&self) -> String {
        let mut uri = format!("{SCHEME}?code={}", self.code);
        if let Some(peer) = &self.peer {
            uri.push_str(&format!("&id={}", peer.to_hex()));
        }
        if let Some(name) = &self.name {
            uri.push_str(&format!("&name={}", percent_encode(name)));
        }
        if let Some(at) = &self.at {
            uri.push_str(&format!("&at={at}"));
        }
        uri
    }

    /// The code alone ("123456", "123 456") or the QR code's text.
    pub fn parse(text: &str) -> Option<PairInvite> {
        let text = text.trim();
        if let Some(query) = text.strip_prefix(SCHEME).and_then(|rest| rest.strip_prefix('?')) {
            let mut invite = PairInvite { code: String::new(), peer: None, name: None, at: None };
            for part in query.split('&') {
                let (key, value) = part.split_once('=')?;
                match key {
                    "code" => invite.code = normalise_code(value)?,
                    "id" => invite.peer = Some(PeerId::from_hex(value).ok()?),
                    "name" => invite.name = Some(percent_decode(value)?),
                    "at" => invite.at = Some(value.to_string()),
                    _ => {}
                }
            }
            return (!invite.code.is_empty()).then_some(invite);
        }
        Some(PairInvite { code: normalise_code(text)?, peer: None, name: None, at: None })
    }
}

fn normalise_code(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    (digits.len() == 6 && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A new pairing code: windowcast's 6 digits.
pub fn new_code() -> String {
    windowcast_pairing::generate_pin()
}

/// What kind of device a paired one is: a computer (droidtop-agent or a
/// desktop manager such as Pythia) or a handheld (droidtop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Computer,
    Handheld,
}

/// The device paired with, as each side records it.
#[derive(Debug, Clone)]
pub struct Paired {
    pub peer: PeerId,
    pub name: String,
    pub kind: Kind,
    /// Its certificate's fingerprint, in hex ([`crate::cert::fingerprint`]).
    pub fingerprint: String,
}

/// This exchange's label in every transcript, so a transcript from
/// windowcast's signaling can never stand in for one of these.
const LABEL: &[u8] = b"droidtop-agent pair v3\0";

/// The version of this exchange: 3 carries the certificate; a version-1
/// hello has none.
const VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct Hello {
    v: u32,
    id: String,
    nonce: String,
    spake: String,
}

/// A side's description ([`About`], as the exact JSON text the proof
/// signs), and its proof over the transcript that carries it.
#[derive(Serialize, Deserialize)]
struct Description {
    about: String,
    signature: String,
    tag: String,
}

#[derive(Serialize, Deserialize)]
struct About {
    name: String,
    kind: Kind,
    /// The certificate's fingerprint, in hex.
    cert: String,
}

fn send<T: Serialize>(stream: &mut impl Write, value: &T) -> Result<()> {
    write_frame(stream, &serde_json::to_vec(value)?)?;
    Ok(())
}

fn recv<T: for<'de> Deserialize<'de>>(stream: &mut impl Read) -> Result<T> {
    Ok(serde_json::from_slice(&read_frame(stream, MAX_PLAIN_FRAME)?)?)
}

fn clip(name: &str) -> String {
    name.chars().take(MAX_NAME).collect()
}

fn hello(key: &DeviceKey, spake: &[u8]) -> (Hello, exchange::Hello) {
    let mine = exchange::Hello::new(key.peer_id(), 0);
    (Hello { v: VERSION, id: key.peer_id().to_hex(), nonce: hex::encode(&mine.nonce), spake: hex::encode(spake) }, mine)
}

/// The other side's hello, and its SPAKE2 message.
fn their_hello(stream: &mut impl Read) -> Result<(exchange::Hello, Vec<u8>)> {
    let value: serde_json::Value = recv(stream)?;
    if value.get("v").and_then(|v| v.as_u64()) != Some(VERSION as u64) {
        return Err(Error::Pairing("the other device runs an older droidtop-agent or droidtop; update both and pair again".into()));
    }
    let hello: Hello = serde_json::from_value(value)?;
    let peer = PeerId::from_hex(&hello.id).map_err(|_| Error::Pairing("a malformed device id".into()))?;
    crate::keys::x25519_public_of(&peer)?;
    let nonce: [u8; 32] =
        hex::decode(&hello.nonce).and_then(|b| b.try_into().ok()).ok_or_else(|| Error::Pairing("a malformed hello".into()))?;
    let spake = hex::decode(&hello.spake).ok_or_else(|| Error::Pairing("a malformed key exchange".into()))?;
    Ok((exchange::Hello { peer, nonce, mode: 0 }, spake))
}

/// Sends this side's description with its proof; [`side`] is 0 for the
/// side the code was typed on (the client), 1 for the side showing it (the host).
fn describe(
    stream: &mut impl Write,
    key: &DeviceKey,
    session: &SessionKey,
    side: u8,
    hellos: (&exchange::Hello, &exchange::Hello),
    name: &str,
    kind: Kind,
) -> Result<()> {
    let about = serde_json::to_string(&About { name: clip(name), kind, cert: cert::fingerprint_hex(&key.peer_id()) })?;
    let t = exchange::transcript(LABEL, side, hellos.0, hellos.1, &[0; 32], about.as_bytes());
    let proof = exchange::prove(&Identity::from_seed(&key.seed()), Some(session), &t);
    let tag = proof.pin_tag.map(|t| hex::encode(&t)).unwrap_or_default();
    send(stream, &Description { about, signature: hex::encode(&proof.signature), tag })
}

/// Reads the other side's description, checks its proof and that its
/// certificate is the one of the key it proved with.
fn described(stream: &mut impl Read, session: &SessionKey, side: u8, hellos: (&exchange::Hello, &exchange::Hello)) -> Result<Paired> {
    let d: Description = recv(stream)?;
    let signer = if side == 0 { hellos.0.peer } else { hellos.1.peer };
    let t = exchange::transcript(LABEL, side, hellos.0, hellos.1, &[0; 32], d.about.as_bytes());
    let proof =
        Proof { signature: hex::decode(&d.signature).unwrap_or_default(), pin_tag: hex::decode(&d.tag).and_then(|b| b.try_into().ok()) };
    exchange::check(&signer, Some(session), &t, &proof).map_err(|_| Error::Pairing("the code did not match".into()))?;
    let about: About = serde_json::from_str(&d.about).map_err(|_| Error::Pairing("a malformed description".into()))?;
    let fingerprint = cert::fingerprint_hex(&signer);
    if about.cert != fingerprint {
        return Err(Error::Pairing("the other device's certificate is not the one of its key; nothing was paired".into()));
    }
    Ok(Paired { peer: signer, name: clip(&about.name), kind: about.kind, fingerprint })
}

/// The side that shows [`code`]; the other side connected to it. [`kind`]
/// is what this device is.
pub fn pair_host<S: Read + Write>(stream: &mut S, key: &DeviceKey, name: &str, kind: Kind, code: &str) -> Result<Paired> {
    let start = start_host(code);
    let (client, spake) = their_hello(stream)?;
    let (mine, host) = hello(key, &start.outbound_message);
    send(stream, &mine)?;
    let session = finish(start, &spake).map_err(|e| Error::Pairing(e.to_string()))?;
    let paired = described(stream, &session, 0, (&client, &host))?;
    describe(stream, key, &session, 1, (&client, &host), name, kind)?;
    Ok(paired)
}

/// The side the person typed [`code`] on. [`kind`] is what this device is.
pub fn pair_client<S: Read + Write>(stream: &mut S, key: &DeviceKey, name: &str, kind: Kind, code: &str) -> Result<Paired> {
    let start = start_client(code);
    let (mine, client) = hello(key, &start.outbound_message);
    send(stream, &mine)?;
    let (host, spake) = their_hello(stream)?;
    let session = finish(start, &spake).map_err(|e| Error::Pairing(e.to_string()))?;
    describe(stream, key, &session, 0, (&client, &host), name, kind)?;
    described(stream, &session, 1, (&client, &host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn run(host_code: &str, client_code: &str) -> (Result<Paired>, Result<Paired>, PeerId, PeerId) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let host_key = DeviceKey::generate();
        let client_key = DeviceKey::generate();
        let (host_id, client_id) = (host_key.peer_id(), client_key.peer_id());
        let host_code = host_code.to_string();
        let host = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            pair_host(&mut s, &host_key, "Handheld", Kind::Handheld, &host_code)
        });
        let mut s = TcpStream::connect(addr).unwrap();
        let client = pair_client(&mut s, &client_key, "Desktop", Kind::Computer, client_code);
        drop(s);
        (host.join().unwrap(), client, host_id, client_id)
    }

    #[test]
    fn matching_codes_pair_both_sides() {
        let (host, client, host_id, client_id) = run("123456", "123456");
        let (host, client) = (host.unwrap(), client.unwrap());
        assert_eq!(host.peer, client_id);
        assert_eq!(host.name, "Desktop");
        assert_eq!(host.kind, Kind::Computer);
        assert_eq!(host.fingerprint, cert::fingerprint_hex(&client_id));
        assert_eq!(client.peer, host_id);
        assert_eq!(client.name, "Handheld");
        assert_eq!(client.kind, Kind::Handheld);
        assert_eq!(client.fingerprint, cert::fingerprint_hex(&host_id));
    }

    #[test]
    fn a_wrong_code_fails_on_both_sides() {
        let (host, client, _, _) = run("123456", "654321");
        assert!(host.is_err());
        assert!(client.is_err());
    }

    #[test]
    fn invites_parse_from_a_code_or_the_qr_text() {
        assert_eq!(PairInvite::parse("123 456").unwrap().code, "123456");
        assert!(PairInvite::parse("12345").is_none());
        let key = DeviceKey::generate();
        let invite = PairInvite {
            code: "042042".into(),
            peer: Some(key.peer_id()),
            name: Some("Retroid Pocket 5".into()),
            at: Some("192.168.1.20:47611".into()),
        };
        assert_eq!(PairInvite::parse(&invite.to_uri()).unwrap(), invite);
    }
}
