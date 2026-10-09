//! Pairing a computer with a handheld (docs/DESIGN.md section 3): windowcast's
//! SPAKE2 run over a plain TCP stream, then each side proves it derived the
//! same key with an HMAC tag over the transcript (both PeerIds and both
//! names), and pins the other's PeerId.
//!
//! The handheld shows the 6-digit code, so it is the SPAKE2 host; the
//! computer, where the code is typed, is the client and speaks first.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use windowcast_pairing::{authenticate_fingerprint, finish, start_client, start_host, verify_fingerprint, SessionKey};

use crate::frame::{read_frame, write_frame, MAX_PLAIN_FRAME};
use crate::keys::{DeviceKey, PeerId};
use crate::{hex, Error, Result};

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

/// The device paired with, as each side records it.
#[derive(Debug, Clone)]
pub struct Paired {
    pub peer: PeerId,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct Hello {
    id: String,
    name: String,
    spake: String,
}

#[derive(Serialize, Deserialize)]
struct Tag {
    tag: String,
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

/// What both tags cover, with the side that made the tag in front, so a tag
/// cannot be reflected back at the side that sent it.
fn transcript(side: &[u8], host: &PeerId, client: &PeerId, host_name: &str, client_name: &str) -> Vec<u8> {
    let mut t = b"droidtop-agent pair v1\0".to_vec();
    t.extend_from_slice(side);
    t.extend_from_slice(&host.0);
    t.extend_from_slice(&client.0);
    for name in [host_name, client_name] {
        t.extend_from_slice(&(name.len() as u32).to_be_bytes());
        t.extend_from_slice(name.as_bytes());
    }
    t
}

fn tag(key: &SessionKey, transcript: &[u8]) -> Tag {
    Tag { tag: hex::encode(&authenticate_fingerprint(key, transcript)) }
}

fn check(key: &SessionKey, transcript: &[u8], tag: &Tag) -> Result<()> {
    let bytes: [u8; 32] =
        hex::decode(&tag.tag).and_then(|b| b.try_into().ok()).ok_or_else(|| Error::Pairing("a malformed confirmation".into()))?;
    verify_fingerprint(key, transcript, &bytes).map_err(|_| Error::Pairing("the code did not match".into()))
}

fn peer_of(hello: &Hello) -> Result<PeerId> {
    let peer = PeerId::from_hex(&hello.id).map_err(|_| Error::Pairing("a malformed device id".into()))?;
    crate::keys::x25519_public_of(&peer)?;
    Ok(peer)
}

/// The handheld's side: it shows [`code`]; the computer connected to it.
pub fn pair_host<S: Read + Write>(stream: &mut S, key: &DeviceKey, name: &str, code: &str) -> Result<Paired> {
    let name = clip(name);
    let start = start_host(code);
    let outbound = start.outbound_message.clone();
    let hello: Hello = recv(stream)?;
    let client = peer_of(&hello)?;
    let client_name = clip(&hello.name);
    send(stream, &Hello { id: key.peer_id().to_hex(), name: name.clone(), spake: hex::encode(&outbound) })?;
    let spake = hex::decode(&hello.spake).ok_or_else(|| Error::Pairing("a malformed key exchange".into()))?;
    let session = finish(start, &spake).map_err(|e| Error::Pairing(e.to_string()))?;
    let host = key.peer_id();
    let client_tag: Tag = recv(stream)?;
    check(&session, &transcript(b"client", &host, &client, &name, &client_name), &client_tag)?;
    send(stream, &tag(&session, &transcript(b"host", &host, &client, &name, &client_name)))?;
    Ok(Paired { peer: client, name: client_name })
}

/// The computer's side: the person typed [`code`] there.
pub fn pair_client<S: Read + Write>(stream: &mut S, key: &DeviceKey, name: &str, code: &str) -> Result<Paired> {
    let name = clip(name);
    let start = start_client(code);
    send(stream, &Hello { id: key.peer_id().to_hex(), name: name.clone(), spake: hex::encode(&start.outbound_message) })?;
    let hello: Hello = recv(stream)?;
    let host = peer_of(&hello)?;
    let host_name = clip(&hello.name);
    let spake = hex::decode(&hello.spake).ok_or_else(|| Error::Pairing("a malformed key exchange".into()))?;
    let session = finish(start, &spake).map_err(|e| Error::Pairing(e.to_string()))?;
    let client = key.peer_id();
    send(stream, &tag(&session, &transcript(b"client", &host, &client, &host_name, &name)))?;
    let host_tag: Tag = recv(stream)?;
    check(&session, &transcript(b"host", &host, &client, &host_name, &name), &host_tag)?;
    Ok(Paired { peer: host, name: host_name })
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
            pair_host(&mut s, &host_key, "Handheld", &host_code)
        });
        let mut s = TcpStream::connect(addr).unwrap();
        let client = pair_client(&mut s, &client_key, "Desktop", client_code);
        drop(s);
        (host.join().unwrap(), client, host_id, client_id)
    }

    #[test]
    fn matching_codes_pair_both_sides() {
        let (host, client, host_id, client_id) = run("123456", "123456");
        let (host, client) = (host.unwrap(), client.unwrap());
        assert_eq!(host.peer, client_id);
        assert_eq!(host.name, "Desktop");
        assert_eq!(client.peer, host_id);
        assert_eq!(client.name, "Handheld");
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
