//! Direct WireGuard between two paired devices (docs/DESIGN.md section 10,
//! transport 2), all in userspace: boringtun's WireGuard state machine over
//! a UDP socket, and smoltcp's TCP over the tunnel's IP packets, so neither
//! device needs a TUN interface, root or a VPN slot. The tunnel's static keys
//! are the devices' own X25519 keys, so only a paired key gets in; the
//! session channel then runs inside it unchanged ([`crate::channel`]).
//!
//! A tunnel carries one point-to-point link: the side that connects is
//! 10.47.61.1, the side that listens 10.47.61.2, and the inner TCP port is
//! the agent's port. The computer listens on UDP [`WG_PORT`]; the handheld
//! tries every endpoint it knows for the computer at once and keeps the one
//! that answers. A tunnel exists only while a sync runs.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant as Clock};

use boringtun::noise::{Tunn, TunnResult};
use rand_core::{OsRng, RngCore};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::keys::{x25519_public_of, DeviceKey, PeerId};

/// The computer's WireGuard port.
pub const WG_PORT: u16 = 47611;

const MTU: usize = 1380;
const INITIATOR_IP: Ipv4Addr = Ipv4Addr::new(10, 47, 61, 1);
const RESPONDER_IP: Ipv4Addr = Ipv4Addr::new(10, 47, 61, 2);
const INNER_PORT: u16 = crate::PORT;
const WIRE: usize = 65_536 + 256;
const TCP_BUFFER: usize = 256 * 1024;
/// A session nobody has used for this long is dropped.
const IDLE: Duration = Duration::from_secs(180);

/// Packets between smoltcp and the tunnel.
#[derive(Default)]
struct VirtualDevice {
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
}

struct RxTok(Vec<u8>);
struct TxTok<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::RxToken for RxTok {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}

impl phy::TxToken for TxTok<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut packet = vec![0u8; len];
        let out = f(&mut packet);
        self.0.push_back(packet);
        out
    }
}

impl Device for VirtualDevice {
    type RxToken<'a>
        = RxTok
    where
        Self: 'a;
    type TxToken<'a>
        = TxTok<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let packet = self.rx.pop_front()?;
        Some((RxTok(packet), TxTok(&mut self.tx)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxTok(&mut self.tx))
    }

    #[allow(clippy::field_reassign_with_default)]
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = MTU;
        caps
    }
}

/// One end of a stream through the tunnel: what the session channel runs on.
pub struct TunnelStream {
    to_loop: Sender<Vec<u8>>,
    from_loop: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
    eof: bool,
    timeout: Option<Duration>,
}

impl TunnelStream {
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }
}

impl Read for TunnelStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.pending.len() {
            if self.eof {
                return Ok(0);
            }
            let next = match self.timeout {
                Some(t) => self.from_loop.recv_timeout(t).map_err(|e| match e {
                    RecvTimeoutError::Timeout => io::Error::new(io::ErrorKind::TimedOut, "the tunnel went quiet"),
                    RecvTimeoutError::Disconnected => io::Error::new(io::ErrorKind::UnexpectedEof, "the tunnel closed"),
                })?,
                None => self.from_loop.recv().map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "the tunnel closed"))?,
            };
            if next.is_empty() {
                self.eof = true;
                return Ok(0);
            }
            self.pending = next;
            self.pos = 0;
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for TunnelStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.to_loop.send(buf.to_vec()).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the tunnel closed"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The loop's side of a [`TunnelStream`].
struct AppSide {
    to_app: Sender<Vec<u8>>,
    from_app: Receiver<Vec<u8>>,
}

fn stream_pair() -> (TunnelStream, AppSide) {
    let (to_loop, from_app) = mpsc::channel();
    let (to_app, from_loop) = mpsc::channel();
    (TunnelStream { to_loop, from_loop, pending: Vec::new(), pos: 0, eof: false, timeout: None }, AppSide { to_app, from_app })
}

fn ip(a: Ipv4Addr) -> IpAddress {
    let [a, b, c, d] = a.octets();
    IpAddress::v4(a, b, c, d)
}

fn tunn_for(key: &DeviceKey, peer: &PeerId) -> io::Result<Tunn> {
    let secret = StaticSecret::from(key.x25519_secret());
    let public = PublicKey::from(x25519_public_of(peer).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?);
    // boringtun keeps the low 8 bits of the index for its own sessions.
    Ok(Tunn::new(secret, public, None, Some(25), OsRng.next_u32() >> 8, None))
}

/// One WireGuard peer and the TCP link inside it.
struct Session {
    tunn: Tunn,
    endpoint: SocketAddr,
    device: VirtualDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    tcp: SocketHandle,
    initiator: bool,
    app: Option<AppSide>,
    outgoing: Vec<u8>,
    established: bool,
    app_gone: bool,
    heard: bool,
    last_use: Clock,
}

impl Session {
    fn new(tunn: Tunn, endpoint: SocketAddr, initiator: bool) -> Session {
        let mut device = VirtualDevice::default();
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = OsRng.next_u64();
        let mut iface = Interface::new(config, &mut device, Instant::now());
        let own = if initiator { INITIATOR_IP } else { RESPONDER_IP };
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(ip(own), 24));
        });
        let mut sockets = SocketSet::new(Vec::new());
        let socket = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; TCP_BUFFER]), tcp::SocketBuffer::new(vec![0; TCP_BUFFER]));
        let tcp = sockets.add(socket);
        let mut session = Session {
            tunn,
            endpoint,
            device,
            iface,
            sockets,
            tcp,
            initiator,
            app: None,
            outgoing: Vec::new(),
            established: false,
            app_gone: false,
            heard: false,
            last_use: Clock::now(),
        };
        session.open();
        session
    }

    /// Connects (initiator) or listens (responder) on the inner TCP socket.
    fn open(&mut self) {
        let socket = self.sockets.get_mut::<tcp::Socket>(self.tcp);
        if self.initiator {
            let local = 49_152 + (OsRng.next_u32() % 16_000) as u16;
            let _ = socket.connect(self.iface.context(), (ip(RESPONDER_IP), INNER_PORT), local);
        } else {
            let _ = socket.listen(INNER_PORT);
        }
    }

    /// A datagram from the network for this peer.
    fn datagram(&mut self, udp: &UdpSocket, from: SocketAddr, data: &[u8], out: &mut [u8]) -> bool {
        let mut result = self.tunn.decapsulate(Some(from.ip()), data, out);
        let mut ours = false;
        loop {
            match result {
                TunnResult::WriteToNetwork(packet) => {
                    let _ = udp.send_to(packet, from);
                    ours = true;
                    result = self.tunn.decapsulate(None, &[], out);
                }
                TunnResult::WriteToTunnelV4(packet, _) | TunnResult::WriteToTunnelV6(packet, _) => {
                    self.device.rx.push_back(packet.to_vec());
                    ours = true;
                    break;
                }
                TunnResult::Done => break,
                TunnResult::Err(_) => break,
            }
        }
        if ours {
            self.endpoint = from;
            self.heard = true;
            self.last_use = Clock::now();
        }
        ours
    }

    /// Moves data between the app, smoltcp and the network. Returns a new
    /// stream when a responder's inner connection was just accepted.
    fn pump(&mut self, udp: &UdpSocket, out: &mut [u8]) -> Option<TunnelStream> {
        let mut accepted = None;
        if let Some(app) = &self.app {
            loop {
                match app.from_app.try_recv() {
                    Ok(data) => {
                        self.outgoing.extend_from_slice(&data);
                        self.last_use = Clock::now();
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.app_gone = true;
                        break;
                    }
                }
            }
        }
        self.iface.poll(Instant::now(), &mut self.device, &mut self.sockets);
        let socket = self.sockets.get_mut::<tcp::Socket>(self.tcp);
        if socket.state() == tcp::State::Established && !self.established {
            self.established = true;
            if !self.initiator {
                let (stream, app) = stream_pair();
                self.app = Some(app);
                accepted = Some(stream);
            }
        }
        if socket.may_send() && !self.outgoing.is_empty() {
            if let Ok(n) = socket.send_slice(&self.outgoing) {
                self.outgoing.drain(..n);
            }
        }
        let mut chunk = vec![0u8; 16 * 1024];
        while socket.can_recv() {
            match socket.recv_slice(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(app) = &self.app {
                        let _ = app.to_app.send(chunk[..n].to_vec());
                    }
                    self.last_use = Clock::now();
                }
            }
        }
        if self.established && !socket.may_recv() {
            // The other side closed: end of stream for the app, then a fresh link.
            if let Some(app) = self.app.take() {
                let _ = app.to_app.send(Vec::new());
            }
            if !socket.is_active() {
                self.established = false;
                self.outgoing.clear();
                if !self.initiator {
                    socket.abort();
                    self.iface.poll(Instant::now(), &mut self.device, &mut self.sockets);
                    self.open();
                }
            }
        }
        if self.app_gone && self.outgoing.is_empty() {
            let socket = self.sockets.get_mut::<tcp::Socket>(self.tcp);
            socket.close();
        }
        self.iface.poll(Instant::now(), &mut self.device, &mut self.sockets);
        while let Some(packet) = self.device.tx.pop_front() {
            if let TunnResult::WriteToNetwork(data) = self.tunn.encapsulate(&packet, out) {
                let _ = udp.send_to(data, self.endpoint);
            }
        }
        if let TunnResult::WriteToNetwork(data) = self.tunn.update_timers(out) {
            let _ = udp.send_to(data, self.endpoint);
        }
        accepted
    }

    fn finished(&self) -> bool {
        (self.app_gone && !self.sockets.get::<tcp::Socket>(self.tcp).is_active()) || self.last_use.elapsed() > IDLE
    }
}

fn quiet(e: &io::Error) -> bool {
    // Windows reports an ICMP "port unreachable" from an earlier send as a
    // reset on the next receive; it says nothing about this socket.
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::ConnectionReset | io::ErrorKind::Interrupted)
}

/// The handheld's side: a stream to [`peer`] through a WireGuard tunnel,
/// trying every endpoint in [`candidates`] at once. Fails after [`timeout`]
/// when none answers.
pub fn connect(key: &DeviceKey, peer: &PeerId, candidates: &[SocketAddr], timeout: Duration) -> io::Result<TunnelStream> {
    if candidates.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no endpoint is known for the computer"));
    }
    let v6 = candidates.iter().any(SocketAddr::is_ipv6);
    let udp = UdpSocket::bind(if v6 { "[::]:0" } else { "0.0.0.0:0" })?;
    udp.set_read_timeout(Some(Duration::from_millis(5)))?;
    let targets: Vec<SocketAddr> = candidates
        .iter()
        .map(|c| match (v6, c) {
            (true, SocketAddr::V4(a)) => SocketAddr::new(a.ip().to_ipv6_mapped().into(), a.port()),
            _ => *c,
        })
        .collect();
    let mut session = Session::new(tunn_for(key, peer)?, targets[0], true);
    let (stream, app) = stream_pair();
    session.app = Some(app);
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let give_up = Arc::new(AtomicBool::new(false));
    let stop = give_up.clone();
    thread::spawn(move || {
        let mut buf = vec![0u8; WIRE];
        let mut out = vec![0u8; WIRE];
        let mut init: Vec<u8> = Vec::new();
        let mut last_probe: Option<Clock> = None;
        let mut probes = 0u32;
        let mut told = false;
        loop {
            if stop.load(Ordering::Relaxed) || session.finished() {
                break;
            }
            // Until one endpoint answers, every candidate gets the handshake:
            // the same one each second, a new one every fourth.
            if !session.heard && last_probe.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
                if probes % 4 == 0 {
                    if let TunnResult::WriteToNetwork(packet) = session.tunn.format_handshake_initiation(&mut out, true) {
                        init = packet.to_vec();
                    }
                }
                for t in &targets {
                    let _ = udp.send_to(&init, *t);
                }
                probes += 1;
                last_probe = Some(Clock::now());
            }
            match udp.recv_from(&mut buf) {
                Ok((n, from)) => {
                    session.datagram(&udp, from, &buf[..n], &mut out);
                }
                Err(e) if quiet(&e) => {}
                Err(_) => break,
            }
            session.pump(&udp, &mut out);
            if session.established && !told {
                told = true;
                let _ = ready_tx.send(());
            }
        }
    });
    match ready_rx.recv_timeout(timeout) {
        Ok(()) => Ok(stream),
        Err(_) => {
            give_up.store(true, Ordering::Relaxed);
            Err(io::Error::new(io::ErrorKind::TimedOut, "the computer did not answer through WireGuard"))
        }
    }
}

/// The computer's side: answers WireGuard from [`peers`] (the paired
/// devices, asked afresh for each new handshake) on [`udp`], and hands each
/// inner connection to [`on_stream`]. Runs until [`stop`].
pub fn serve(
    key: &DeviceKey,
    udp: UdpSocket,
    peers: impl Fn() -> Vec<PeerId>,
    on_stream: impl Fn(PeerId, TunnelStream),
    stop: &AtomicBool,
) -> io::Result<()> {
    udp.set_read_timeout(Some(Duration::from_millis(5)))?;
    let mut sessions: HashMap<PeerId, Session> = HashMap::new();
    let mut buf = vec![0u8; WIRE];
    let mut out = vec![0u8; WIRE];
    while !stop.load(Ordering::Relaxed) {
        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                let data = &buf[..n];
                let known = sessions.iter_mut().find(|(_, s)| s.endpoint == from).map(|(p, _)| *p);
                // A new handshake on a link with no stream is a device that
                // started over (a new sync): it gets a fresh session. A live
                // link's handshakes are rekeys and stay with it.
                let initiation = data.first() == Some(&1);
                if let Some(peer) = known {
                    if initiation && sessions.get(&peer).is_some_and(|s| s.app.is_none()) {
                        sessions.remove(&peer);
                    }
                }
                let known = known.filter(|p| sessions.contains_key(p));
                let handled = match known {
                    Some(peer) => sessions.get_mut(&peer).is_some_and(|s| s.datagram(&udp, from, data, &mut out)),
                    None => false,
                };
                if !handled {
                    // A new endpoint: a session that moved, or a new handshake from a paired device.
                    let roamed = sessions.iter_mut().find_map(|(p, s)| s.datagram(&udp, from, data, &mut out).then_some(*p));
                    if roamed.is_none() {
                        for peer in peers() {
                            if sessions.contains_key(&peer) {
                                continue;
                            }
                            let Ok(tunn) = tunn_for(key, &peer) else { continue };
                            let mut session = Session::new(tunn, from, false);
                            if session.datagram(&udp, from, data, &mut out) {
                                sessions.insert(peer, session);
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) if quiet(&e) => {}
            Err(e) => return Err(e),
        }
        for (peer, session) in sessions.iter_mut() {
            if let Some(stream) = session.pump(&udp, &mut out) {
                on_stream(*peer, stream);
            }
        }
        sessions.retain(|_, s| s.last_use.elapsed() <= IDLE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;

    #[test]
    fn a_session_channel_runs_through_the_tunnel() {
        let pc = DeviceKey::generate();
        let pc_id = pc.peer_id();
        let handheld = DeviceKey::generate();
        let hh_id = handheld.peer_id();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = udp.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = stop.clone();
        let pc_seed = pc.seed();
        let server = thread::spawn(move || {
            let pc = DeviceKey::from_seed(&pc_seed).unwrap();
            serve(
                &pc,
                udp,
                move || vec![hh_id],
                move |peer, mut stream| {
                    assert_eq!(peer, hh_id);
                    // The loop keeps pumping; the session runs on its own thread.
                    thread::spawn(move || {
                        let key = DeviceKey::from_seed(&pc_seed).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(20)));
                        let mut ch = Channel::accept(stream, &key, |p| *p == hh_id).unwrap();
                        let got = ch.recv().unwrap();
                        ch.send(&got).unwrap();
                    });
                },
                &stop_server,
            )
            .unwrap();
        });
        let wrong = SocketAddr::from(([127, 0, 0, 1], 9));
        let target = SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream = connect(&handheld, &pc_id, &[wrong, target], Duration::from_secs(20)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20)));
        let mut ch = Channel::connect(stream, &handheld, &pc_id).unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        ch.send(&big).unwrap();
        assert_eq!(ch.recv().unwrap(), big);
        drop(ch);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }

    #[test]
    fn an_unpaired_key_gets_no_answer() {
        let pc = DeviceKey::generate();
        let pc_id = pc.peer_id();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = udp.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = stop.clone();
        let paired = DeviceKey::generate().peer_id();
        let server = thread::spawn(move || {
            serve(&pc, udp, move || vec![paired], |_, _| panic!("a stranger got a stream"), &stop_server).unwrap();
        });
        let stranger = DeviceKey::generate();
        let result = connect(&stranger, &pc_id, &[SocketAddr::from(([127, 0, 0, 1], port))], Duration::from_secs(3));
        assert!(result.is_err());
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }
}
