//! The session channel (docs/DESIGN.md section 4): Noise_IK between two paired
//! keys over any byte stream (TCP on the LAN, TCP inside the WireGuard
//! tunnel). The handheld initiates: it knows the computer's key from
//! pairing. The computer learns the handheld's PeerId from the encrypted
//! payload of the first message, checks it matches the static key Noise
//! authenticated, and refuses it unless it is pinned.
//!
//! An application message is a 4-byte length and its bytes, split across as
//! many Noise messages as it needs.

use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use snow::{Builder, TransportState};

use crate::frame::{read_short, write_short};
use crate::keys::{x25519_public_of, DeviceKey, PeerId};
use crate::{error::protocol, Error, Result};

const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"droidtop-agent/1";
const MAX_NOISE: usize = 65535;
const TAG: usize = 16;
const MAX_PLAIN: usize = MAX_NOISE - TAG;

/// The largest application message: a large library list fits many times.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

pub struct Channel<S> {
    stream: S,
    noise: TransportState,
    peer: PeerId,
    out: Vec<u8>,
    inp: Vec<u8>,
}

fn builder<'a>(secret: &'a [u8; 32]) -> Builder<'a> {
    Builder::new(PATTERN.parse().expect("a valid Noise pattern")).local_private_key(secret).prologue(PROLOGUE)
}

impl<S: Read + Write> Channel<S> {
    /// The handheld's side: open a channel to the computer [`server`].
    pub fn connect(mut stream: S, key: &DeviceKey, server: &PeerId) -> Result<Self> {
        let secret = key.x25519_secret();
        let remote = x25519_public_of(server)?;
        let mut hs = builder(&secret).remote_public_key(&remote).build_initiator()?;
        let mut buf = vec![0u8; MAX_NOISE];
        let n = hs.write_message(&key.peer_id().0, &mut buf)?;
        write_short(&mut stream, &buf[..n])?;
        stream.flush()?;
        let reply = read_short(&mut stream)?;
        let mut payload = vec![0u8; MAX_NOISE];
        hs.read_message(&reply, &mut payload)?;
        let noise = hs.into_transport_mode()?;
        Ok(Channel { stream, noise, peer: *server, out: buf, inp: payload })
    }

    /// The computer's side: accept a channel from a device [`trusted`] says is paired.
    pub fn accept(mut stream: S, key: &DeviceKey, trusted: impl Fn(&PeerId) -> bool) -> Result<Self> {
        let secret = key.x25519_secret();
        let mut hs = builder(&secret).build_responder()?;
        let first = read_short(&mut stream)?;
        let mut payload = vec![0u8; MAX_NOISE];
        let n = hs.read_message(&first, &mut payload)?;
        if n != 32 {
            return Err(Error::NotPaired);
        }
        let peer = PeerId(payload[..32].try_into().expect("32 bytes"));
        let remote = hs.get_remote_static().ok_or(Error::NotPaired)?;
        if x25519_public_of(&peer)?[..] != *remote || !trusted(&peer) {
            return Err(Error::NotPaired);
        }
        let mut buf = vec![0u8; MAX_NOISE];
        let n = hs.write_message(&[], &mut buf)?;
        write_short(&mut stream, &buf[..n])?;
        stream.flush()?;
        let noise = hs.into_transport_mode()?;
        Ok(Channel { stream, noise, peer, out: buf, inp: payload })
    }

    /// The device on the other end.
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_MESSAGE {
            return Err(protocol("a message too large to send"));
        }
        let mut piece = Vec::with_capacity(MAX_PLAIN);
        piece.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut offset = 0;
        loop {
            let end = (offset + MAX_PLAIN - piece.len()).min(data.len());
            piece.extend_from_slice(&data[offset..end]);
            offset = end;
            let n = self.noise.write_message(&piece, &mut self.out)?;
            write_short(&mut self.stream, &self.out[..n])?;
            piece.clear();
            if offset >= data.len() {
                break;
            }
        }
        self.stream.flush()?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Vec<u8>> {
        let first = read_short(&mut self.stream)?;
        let n = self.noise.read_message(&first, &mut self.inp)?;
        if n < 4 {
            return Err(protocol("a truncated message"));
        }
        let total = u32::from_be_bytes(self.inp[..4].try_into().expect("4 bytes")) as usize;
        if total > MAX_MESSAGE {
            return Err(protocol("a message too large to receive"));
        }
        let mut data = Vec::with_capacity(total);
        data.extend_from_slice(&self.inp[4..n]);
        while data.len() < total {
            let next = read_short(&mut self.stream)?;
            let n = self.noise.read_message(&next, &mut self.inp)?;
            data.extend_from_slice(&self.inp[..n]);
        }
        if data.len() != total {
            return Err(protocol("a message longer than it said"));
        }
        Ok(data)
    }

    pub fn send_json<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.send(&bytes)
    }

    pub fn recv_json<T: DeserializeOwned>(&mut self) -> Result<T> {
        let bytes = self.recv()?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    fn a_paired_device_talks_and_large_messages_survive() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = DeviceKey::generate();
        let client = DeviceKey::generate();
        let server_id = server.peer_id();
        let client_id = client.peer_id();
        let handle = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut ch = Channel::accept(s, &server, |p| *p == client_id).unwrap();
            assert_eq!(ch.peer(), client_id);
            let got = ch.recv().unwrap();
            ch.send(&got).unwrap();
            let empty = ch.recv().unwrap();
            assert!(empty.is_empty());
        });
        let mut ch = Channel::connect(TcpStream::connect(addr).unwrap(), &client, &server_id).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        ch.send(&big).unwrap();
        assert_eq!(ch.recv().unwrap(), big);
        ch.send(&[]).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn an_unpaired_device_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = DeviceKey::generate();
        let server_id = server.peer_id();
        let handle = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            Channel::accept(s, &server, |_| false).err()
        });
        let stranger = DeviceKey::generate();
        let _ = Channel::connect(TcpStream::connect(addr).unwrap(), &stranger, &server_id);
        assert!(matches!(handle.join().unwrap(), Some(Error::NotPaired)));
    }

    #[test]
    fn connecting_to_the_wrong_key_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = DeviceKey::generate();
        let handle = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            Channel::accept(s, &server, |_| true).is_err()
        });
        let client = DeviceKey::generate();
        let impostor = DeviceKey::generate().peer_id();
        assert!(Channel::connect(TcpStream::connect(addr).unwrap(), &client, &impostor).is_err());
        assert!(handle.join().unwrap());
    }
}
