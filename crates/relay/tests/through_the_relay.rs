//! A WireGuard tunnel between two paired keys through the relay, with neither side reaching the
//! other directly: the computer's tunnel (lib-sync's `tunnel::serve`) and the handheld's
//! (`tunnel::connect`) are the ones a direct call uses, unchanged. The relay sees only ciphertext,
//! forwards nothing to a key that did not allow the sender, and takes no one who cannot prove a key.

use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::tunnel::{self, NoHooks};
use droidtop_agent_relay::{hello_message, serve, Bridge, Frame, Relay};

type Seen = Arc<Mutex<Vec<(PeerId, PeerId, Vec<u8>)>>>;

fn relay() -> (String, Relay, Seen) {
    let seen: Seen = Arc::default();
    let tap = seen.clone();
    let relay = Relay::with_tap(move |from, to, packet| tap.lock().unwrap().push((*from, *to, packet.to_vec())));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = listener.local_addr().unwrap().to_string();
    let serving = relay.clone();
    thread::spawn(move || serve(&serving, listener, &AtomicBool::new(false)));
    (at, relay, seen)
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// The computer: WireGuard on a socket only the relay's bridge reaches, echoing each inner stream.
fn computer(key: &DeviceKey, handheld: PeerId) -> std::net::SocketAddr {
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let at = udp.local_addr().unwrap();
    let key = DeviceKey::from_seed(&key.seed()).unwrap();
    thread::spawn(move || {
        let stop = AtomicBool::new(false);
        let _ = tunnel::serve(
            &key,
            udp,
            move || vec![handheld],
            |_, mut stream| {
                thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut buf) {
                        if n == 0 || stream.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = stream.flush();
                    }
                });
            },
            &stop,
            &mut NoHooks,
        );
    });
    at
}

#[test]
fn a_tunnel_runs_through_the_relay_and_the_relay_sees_only_ciphertext() {
    let (at, relay, seen) = relay();
    let (pc, handheld) = (DeviceKey::generate(), DeviceKey::generate());
    let wg = computer(&pc, handheld.peer_id());

    let _pc_bridge = Bridge::connect(&at, &pc, vec![handheld.peer_id()], Some(wg), Duration::from_secs(5)).unwrap();
    let hh_bridge = Bridge::connect(&at, &handheld, vec![pc.peer_id()], None, Duration::from_secs(5)).unwrap();
    wait_until("both on the relay", || relay.is_connected(&pc.peer_id()) && relay.is_connected(&handheld.peer_id()));

    // Two calls at once, each with its own session through the relay (Droidtop/tracker#529).
    let secret = b"a save file nobody but the computer reads";
    let mut calls = Vec::new();
    for n in 0..2u8 {
        let endpoint = hh_bridge.dial(&pc.peer_id()).unwrap();
        let mut stream = tunnel::connect(&handheld, &pc.peer_id(), &[endpoint], Duration::from_secs(10)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10)));
        calls.push((n, stream));
    }
    for (n, stream) in &mut calls {
        let message = [&secret[..], &[*n]].concat();
        stream.write_all(&message).unwrap();
        stream.flush().unwrap();
        let mut back = vec![0u8; message.len()];
        stream.read_exact(&mut back).unwrap();
        assert_eq!(back, message);
    }

    // What went through the relay: WireGuard packets between the two keys, none holding the plaintext.
    let seen = seen.lock().unwrap();
    assert!(seen.len() >= 4);
    for (from, to, packet) in seen.iter() {
        assert!([pc.peer_id(), handheld.peer_id()].contains(from) && [pc.peer_id(), handheld.peer_id()].contains(to) && from != to);
        assert!((1..=4).contains(&packet[0]), "not a WireGuard message: type {}", packet[0]);
        assert!(!packet.windows(secret.len()).any(|w| w == secret));
    }
    // Neither side's secret key went through it either.
    for k in [&pc, &handheld] {
        let x = k.x25519_secret();
        let seed = k.seed();
        assert!(!seen.iter().any(|(_, _, p)| p.windows(32).any(|w| w == x || w == seed)));
    }
}

/// A raw client: proves [`key`] and allows [`allowed`].
fn raw(at: &str, key: &DeviceKey, allowed: Vec<PeerId>) -> (TcpStream, BufReader<TcpStream>) {
    let mut s = TcpStream::connect(at).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let Frame::Challenge(c) = Frame::read_from(&mut r).unwrap() else { panic!("no challenge") };
    Frame::Hello { key: key.peer_id(), signature: key.sign(&hello_message(&c)) }.write_to(&mut s).unwrap();
    Frame::Allow(allowed).write_to(&mut s).unwrap();
    (s, r)
}

#[test]
fn packets_reach_only_a_key_that_allows_the_sender() {
    let (at, relay, _) = relay();
    let (a, b, stranger) = (DeviceKey::generate(), DeviceKey::generate(), DeviceKey::generate());
    let (_a, mut a_in) = raw(&at, &a, vec![b.peer_id()]);
    let (mut b_out, _b_in) = raw(&at, &b, vec![a.peer_id()]);
    let (mut s_out, _s_in) = raw(&at, &stranger, vec![]);
    wait_until("all three on the relay", || [&a, &b, &stranger].iter().all(|k| relay.is_connected(&k.peer_id())));

    Frame::Send { to: a.peer_id(), flow: 1, packet: b"from a stranger".to_vec() }.write_to(&mut s_out).unwrap();
    Frame::Send { to: a.peer_id(), flow: 2, packet: b"from b".to_vec() }.write_to(&mut b_out).unwrap();
    // Frames keep their order on the relay's queue, so the first a hears is b's: the stranger's was dropped.
    a_in.get_ref().set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(Frame::read_from(&mut a_in).unwrap(), Frame::Recv { from: b.peer_id(), flow: 2, packet: b"from b".to_vec() });
    Frame::Send { to: a.peer_id(), flow: 3, packet: b"again".to_vec() }.write_to(&mut b_out).unwrap();
    assert_eq!(Frame::read_from(&mut a_in).unwrap(), Frame::Recv { from: b.peer_id(), flow: 3, packet: b"again".to_vec() });
}

#[test]
fn a_device_that_cannot_prove_its_key_is_turned_away() {
    let (at, relay, _) = relay();
    let (claimed, actual) = (DeviceKey::generate(), DeviceKey::generate());
    let mut s = TcpStream::connect(&at).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let Frame::Challenge(c) = Frame::read_from(&mut r).unwrap() else { panic!("no challenge") };
    // Someone else's key, signed with one's own.
    Frame::Hello { key: claimed.peer_id(), signature: actual.sign(&hello_message(&c)) }.write_to(&mut s).unwrap();
    r.get_ref().set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut rest = Vec::new();
    // The relay closes the connection without a word.
    assert!(matches!(r.read_to_end(&mut rest), Ok(0) | Err(_)));
    assert!(!relay.is_connected(&claimed.peer_id()));
    // A bridge with the wrong relay address is an error, not a hang.
    let gone = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    assert!(Bridge::connect(gone, &actual, vec![], None, Duration::from_secs(2)).is_err());
}
