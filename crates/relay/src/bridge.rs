//! A device's side of the relay: one TCP connection to it, and a loopback UDP socket per call that
//! the WireGuard tunnel talks to as if it were the other device.
//!
//! - The side that calls ([`Bridge::dial`]) gets an address to give `tunnel::connect` as the peer's
//!   endpoint.
//! - The side that answers ([`Bridge::connect`] with `answer_to`, the computer's WireGuard socket)
//!   gets a new loopback socket for each call that arrives, sending to that socket, so
//!   `tunnel::serve` sees each call from an endpoint of its own.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::keys::{DeviceKey, PeerId};
use rand_core::{OsRng, RngCore};

use crate::{hello_message, Frame, MAX_PACKET};

/// A call nobody has sent on for this long is closed: the tunnel's own idle limit.
const FLOW_IDLE: Duration = Duration::from_secs(180);
/// How often the bridge tells the relay its allow list again, which also keeps the connection alive.
const KEEPALIVE: Duration = Duration::from_secs(60);

struct Flow {
    socket: Arc<UdpSocket>,
    /// Where packets from the relay go: the tunnel's socket.
    reply_to: Arc<Mutex<Option<SocketAddr>>>,
    last: Arc<Mutex<Instant>>,
    closed: Arc<AtomicBool>,
}

struct Inner {
    writer: Mutex<TcpStream>,
    flows: Mutex<HashMap<(PeerId, u32), Flow>>,
    allowed: Mutex<Vec<PeerId>>,
    answer_to: Option<SocketAddr>,
    alive: AtomicBool,
}

/// This device's connection to a relay.
pub struct Bridge {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Bridge {
    /// Connects to the relay at [`relay`] as [`key`]'s device, taking packets from the keys in
    /// [`allowed`]. With [`answer_to`], calls that arrive from an allowed key go to that address
    /// (the computer's WireGuard socket); without it only calls this side dials are carried.
    pub fn connect(
        relay: impl ToSocketAddrs,
        key: &DeviceKey,
        allowed: Vec<PeerId>,
        answer_to: Option<SocketAddr>,
        timeout: Duration,
    ) -> io::Result<Bridge> {
        let addr =
            relay.to_socket_addrs()?.next().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the relay's address is not known"))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(timeout))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let challenge = match Frame::read_from(&mut reader)? {
            Frame::Challenge(c) => c,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "not a droidtop relay")),
        };
        let mut writer = stream.try_clone()?;
        Frame::Hello { key: key.peer_id(), signature: key.sign(&hello_message(&challenge)) }.write_to(&mut writer)?;
        Frame::Allow(allowed.clone()).write_to(&mut writer)?;
        stream.set_read_timeout(None)?;
        let inner = Arc::new(Inner {
            writer: Mutex::new(writer),
            flows: Mutex::new(HashMap::new()),
            allowed: Mutex::new(allowed),
            answer_to,
            alive: AtomicBool::new(true),
        });
        let reading = inner.clone();
        thread::spawn(move || {
            while let Ok(frame) = Frame::read_from(&mut reader) {
                if let Frame::Recv { from, flow, packet } = frame {
                    reading.deliver(from, flow, &packet);
                }
            }
            reading.close();
        });
        let keeping = inner.clone();
        thread::spawn(move || {
            let mut last = Instant::now();
            while keeping.alive.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(500));
                lock(&keeping.flows).retain(|_, f| {
                    let keep = lock(&f.last).elapsed() < FLOW_IDLE;
                    if !keep {
                        f.closed.store(true, Ordering::Relaxed);
                    }
                    keep
                });
                if last.elapsed() >= KEEPALIVE {
                    last = Instant::now();
                    let allowed = lock(&keeping.allowed).clone();
                    if keeping.send(&Frame::Allow(allowed)).is_err() {
                        keeping.close();
                    }
                }
            }
        });
        Ok(Bridge { inner })
    }

    /// A loopback address that stands for [`peer`] on a new call: give it to `tunnel::connect` as
    /// the peer's endpoint.
    pub fn dial(&self, peer: &PeerId) -> io::Result<SocketAddr> {
        if !self.is_alive() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "the relay connection has ended"));
        }
        let flow = OsRng.next_u32();
        self.inner.open_flow(*peer, flow, None)
    }

    /// Replaces the keys this device takes packets from (its paired devices, when they change).
    pub fn set_allowed(&self, allowed: Vec<PeerId>) -> io::Result<()> {
        *lock(&self.inner.allowed) = allowed.clone();
        self.inner.send(&Frame::Allow(allowed))
    }

    /// Whether the connection to the relay is still up.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Relaxed)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.inner.close();
    }
}

impl Inner {
    fn send(&self, frame: &Frame) -> io::Result<()> {
        frame.write_to(&mut *lock(&self.writer))
    }

    fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = lock(&self.writer).shutdown(Shutdown::Both);
        for (_, f) in lock(&self.flows).drain() {
            f.closed.store(true, Ordering::Relaxed);
        }
    }

    /// A packet from the relay, out of the call's loopback socket to the tunnel.
    fn deliver(self: &Arc<Self>, from: PeerId, flow: u32, packet: &[u8]) {
        let found = lock(&self.flows).get(&(from, flow)).map(|f| (f.socket.clone(), f.reply_to.clone(), f.last.clone()));
        let (socket, reply_to, last) = match found {
            Some(f) => f,
            None => {
                // A call this side did not dial: only from an allowed key, and only when it answers.
                let Some(target) = self.answer_to else { return };
                if !lock(&self.allowed).contains(&from) || self.open_flow(from, flow, Some(target)).is_err() {
                    return;
                }
                let flows = lock(&self.flows);
                let Some(f) = flows.get(&(from, flow)) else { return };
                (f.socket.clone(), f.reply_to.clone(), f.last.clone())
            }
        };
        *lock(&last) = Instant::now();
        let target = *lock(&reply_to);
        if let Some(target) = target {
            let _ = socket.send_to(packet, target);
        }
    }

    /// A loopback socket for one call with [`peer`]: what the tunnel sends to it goes to the relay.
    fn open_flow(self: &Arc<Self>, peer: PeerId, flow: u32, reply_to: Option<SocketAddr>) -> io::Result<SocketAddr> {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0")?);
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        let local = socket.local_addr()?;
        let fixed = reply_to.is_some();
        let state = Flow {
            socket: socket.clone(),
            reply_to: Arc::new(Mutex::new(reply_to)),
            last: Arc::new(Mutex::new(Instant::now())),
            closed: Arc::new(AtomicBool::new(false)),
        };
        let (reply_to, last, closed) = (state.reply_to.clone(), state.last.clone(), state.closed.clone());
        lock(&self.flows).insert((peer, flow), state);
        let inner = self.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; MAX_PACKET];
            while !closed.load(Ordering::Relaxed) && inner.alive.load(Ordering::Relaxed) {
                let (n, from) = match socket.recv_from(&mut buf) {
                    Ok(got) => got,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                                | io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        continue
                    }
                    Err(_) => break,
                };
                // Only the tunnel on this computer talks to the loopback socket.
                if !from.ip().is_loopback() {
                    continue;
                }
                if !fixed {
                    *lock(&reply_to) = Some(from);
                }
                *lock(&last) = Instant::now();
                if inner.send(&Frame::Send { to: peer, flow, packet: buf[..n].to_vec() }).is_err() {
                    inner.close();
                    break;
                }
            }
        });
        Ok(local)
    }
}
