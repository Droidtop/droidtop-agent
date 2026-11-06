//! The relay itself: accepts devices that prove their key, and forwards each packet to the key it
//! names when that key allows the sender. It keeps no packet longer than it takes to hand it on, and
//! a device that cannot keep up loses packets, the way UDP would (WireGuard and the TCP inside it
//! recover).

use std::collections::{HashMap, HashSet};
use std::io::{self, ErrorKind};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use droidtop_agent_core::keys::PeerId;
use ed25519_dalek::{Signature, VerifyingKey};
use rand_core::{OsRng, RngCore};

use crate::{hello_message, Frame};

/// How long a device has to answer the challenge.
const HELLO_WITHIN: Duration = Duration::from_secs(10);
/// A device that sends nothing for this long is dropped (the bridge sends its allow list more often).
const IDLE: Duration = Duration::from_secs(300);
/// Frames waiting for one device before more are dropped: bursts of a few hundred packets.
const QUEUE: usize = 512;

type Tap = Box<dyn Fn(&PeerId, &PeerId, &[u8]) + Send + Sync>;

struct Client {
    connection: u64,
    tx: SyncSender<Vec<u8>>,
    allowed: HashSet<PeerId>,
}

/// A relay's state: the devices connected now.
#[derive(Clone, Default)]
pub struct Relay {
    clients: Arc<Mutex<HashMap<PeerId, Client>>>,
    next: Arc<AtomicU64>,
    tap: Option<Arc<Tap>>,
}

impl Relay {
    pub fn new() -> Relay {
        Relay::default()
    }

    /// A relay that shows [`tap`] each packet it forwards (sender, recipient, bytes): what a relay's
    /// operator could see, for tests that check it is only ciphertext.
    pub fn with_tap(tap: impl Fn(&PeerId, &PeerId, &[u8]) + Send + Sync + 'static) -> Relay {
        Relay { tap: Some(Arc::new(Box::new(tap))), ..Relay::default() }
    }

    /// Whether a device with [`key`] is connected now.
    pub fn is_connected(&self, key: &PeerId) -> bool {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).contains_key(key)
    }

    /// Serves one device's connection until it ends.
    pub fn handle(&self, stream: TcpStream) -> io::Result<()> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(HELLO_WITHIN))?;
        let mut challenge = [0u8; 32];
        OsRng.fill_bytes(&mut challenge);
        let mut writer = stream.try_clone()?;
        Frame::Challenge(challenge).write_to(&mut writer)?;
        let mut reader = io::BufReader::new(stream.try_clone()?);
        let key = match Frame::read_from(&mut reader)? {
            Frame::Hello { key, signature } => {
                let verifying = VerifyingKey::from_bytes(&key.0).map_err(|_| refused("not a device key"))?;
                verifying
                    .verify_strict(&hello_message(&challenge), &Signature::from_bytes(&signature))
                    .map_err(|_| refused("the device did not prove its key"))?;
                key
            }
            _ => return Err(refused("a device must say hello first")),
        };
        stream.set_read_timeout(Some(IDLE))?;

        let connection = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE);
        // A newer connection with the same key replaces the older one, whose queue then closes.
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Client { connection, tx, allowed: HashSet::new() });
        let out = stream.try_clone()?;
        let sender = thread::spawn(move || {
            use std::io::Write;
            let mut out = out;
            for frame in rx {
                if out.write_all(&frame).is_err() {
                    break;
                }
            }
            let _ = out.shutdown(Shutdown::Both);
        });

        let result = self.forward(&key, &mut reader);
        {
            let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            if clients.get(&key).is_some_and(|c| c.connection == connection) {
                clients.remove(&key);
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
        let _ = sender.join();
        match result {
            Err(e) if matches!(e.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset) => Ok(()),
            other => other,
        }
    }

    fn forward(&self, from: &PeerId, reader: &mut impl io::Read) -> io::Result<()> {
        loop {
            match Frame::read_from(reader)? {
                Frame::Allow(keys) => {
                    if let Some(me) = self.clients.lock().unwrap_or_else(|e| e.into_inner()).get_mut(from) {
                        me.allowed = keys.into_iter().collect();
                    }
                }
                Frame::Send { to, flow, packet } => {
                    let clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(peer) = clients.get(&to).filter(|c| c.allowed.contains(from)) else { continue };
                    if let Some(tap) = &self.tap {
                        tap(from, &to, &packet);
                    }
                    match peer.tx.try_send(Frame::Recv { from: *from, flow, packet }.encode()) {
                        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
                    }
                }
                // A replaced connection reads on until the device notices; it no longer sends anything.
                Frame::Challenge(_) | Frame::Hello { .. } | Frame::Recv { .. } => {
                    return Err(refused("a device sent a frame only the relay sends"))
                }
            }
        }
    }
}

fn refused(why: &str) -> io::Error {
    io::Error::new(ErrorKind::PermissionDenied, why.to_string())
}

/// Accepts devices on [`listener`] for [`relay`] until [`stop`], each on a thread of its own.
pub fn serve(relay: &Relay, listener: TcpListener, stop: &AtomicBool) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                let relay = relay.clone();
                thread::spawn(move || {
                    let _ = relay.handle(stream);
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(20)),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
