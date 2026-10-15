//! The session channel (docs/DESIGN.md section 4): mutual TLS 1.3 between
//! two paired devices over any byte stream (TCP on the LAN, TCP inside the
//! WireGuard tunnel). Each side presents its own certificate ([`crate::cert`])
//! and checks the other's: the side that connects knows the other's key from
//! pairing and accepts only a certificate carrying it; the side that accepts
//! reads the connecting device's key from its certificate and refuses it
//! unless it is pinned.
//!
//! An application message is a 4-byte length and its bytes.

use std::io::{Read, Write};
use std::sync::Arc;

use rustls::{ClientConnection, ServerConnection, StreamOwned};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::cert;
use crate::keys::{DeviceKey, PeerId};
use crate::{error::protocol, Error, Result};

/// The largest application message: a large library list fits many times.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

enum Tls<S: Read + Write> {
    Client(StreamOwned<ClientConnection, S>),
    Server(StreamOwned<ServerConnection, S>),
}

pub struct Channel<S: Read + Write> {
    tls: Tls<S>,
    peer: PeerId,
}

/// A handshake that could not finish, said the way a person can act on.
fn handshake(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        // An agent or droidtop from before TLS closes the connection at the first bytes.
        return Error::Tls("the other side closed the connection; it may run an older droidtop-agent or droidtop, so update both".into());
    }
    match e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
        Some(rustls::Error::InvalidCertificate(_)) => Error::NotPaired,
        Some(rustls::Error::AlertReceived(rustls::AlertDescription::BadCertificate)) => Error::NotPaired,
        _ => Error::Io(e),
    }
}

impl<S: Read + Write> Channel<S> {
    /// The side that connects (droidtop): open a channel to [`server`], the
    /// device paired with this one.
    pub fn connect(mut stream: S, key: &DeviceKey, server: &PeerId) -> Result<Self> {
        let config = cert::client_config(key, server)?;
        let mut conn = ClientConnection::new(Arc::new(config), cert::server_name(server))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut stream).map_err(handshake)?;
        }
        Ok(Channel { tls: Tls::Client(StreamOwned::new(conn, stream)), peer: *server })
    }

    /// The side that accepts (the computer): a channel from a device [`trusted`] says is paired.
    pub fn accept(stream: S, key: &DeviceKey, trusted: impl Fn(&PeerId) -> bool) -> Result<Self> {
        Self::accept_any(stream, &[key], trusted).map(|(ch, _)| ch)
    }

    /// [`accept`] for a computer that answers to more than one key while it
    /// moves to a new identity ([`crate::moved`]): it presents the
    /// certificate of the key the other side asked for. Returns which of
    /// [`keys`] that was.
    pub fn accept_any(mut stream: S, keys: &[&DeviceKey], trusted: impl Fn(&PeerId) -> bool) -> Result<(Self, usize)> {
        let config = cert::server_config(keys)?;
        let mut conn = ServerConnection::new(Arc::new(config))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut stream).map_err(handshake)?;
        }
        let peer = match conn.peer_certificates() {
            Some([first, ..]) => cert::peer_of(first)?,
            _ => return Err(Error::NotPaired),
        };
        if !trusted(&peer) {
            return Err(Error::NotPaired);
        }
        let index = cert::named_index(keys, conn.server_name());
        Ok((Channel { tls: Tls::Server(StreamOwned::new(conn, stream)), peer }, index))
    }

    /// The device on the other end.
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    fn io(&mut self) -> &mut dyn ReadWrite {
        match &mut self.tls {
            Tls::Client(s) => s,
            Tls::Server(s) => s,
        }
    }

    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_MESSAGE {
            return Err(protocol("a message too large to send"));
        }
        let io = self.io();
        io.write_all(&(data.len() as u32).to_be_bytes())?;
        io.write_all(data)?;
        io.flush()?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Vec<u8>> {
        let io = self.io();
        let mut length = [0u8; 4];
        io.read_exact(&mut length)?;
        let total = u32::from_be_bytes(length) as usize;
        if total > MAX_MESSAGE {
            return Err(protocol("a message too large to receive"));
        }
        let mut data = vec![0u8; total];
        io.read_exact(&mut data)?;
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

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

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
    fn the_wire_is_tls_1_3_with_both_certificates() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = DeviceKey::generate();
        let server_id = server.peer_id();
        // What the client sends first, seen by a plain listener.
        let handle = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut first = [0u8; 3];
            s.read_exact(&mut first).unwrap();
            first
        });
        let client = DeviceKey::generate();
        let _ = Channel::connect(TcpStream::connect(addr).unwrap(), &client, &server_id);
        // A TLS handshake record: a ClientHello, not the old Noise frame.
        assert_eq!(handle.join().unwrap(), [0x16, 0x03, 0x01]);
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
    fn a_computer_moving_keys_answers_to_both() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (new, old) = (DeviceKey::generate(), DeviceKey::generate());
        let old_id = old.peer_id();
        let client = DeviceKey::generate();
        let client_id = client.peer_id();
        let handle = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let (mut ch, which) = Channel::accept_any(s, &[&new, &old], |p| *p == client_id).unwrap();
            ch.send(b"hi").unwrap();
            which
        });
        let mut ch = Channel::connect(TcpStream::connect(addr).unwrap(), &client, &old_id).unwrap();
        assert_eq!(ch.recv().unwrap(), b"hi");
        assert_eq!(handle.join().unwrap(), 1);
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
