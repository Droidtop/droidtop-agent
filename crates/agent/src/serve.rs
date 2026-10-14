//! `droidtop-agent run`: listens for paired handhelds on the LAN, answers
//! their discovery queries, rescans on a timer, and handles store-and-forward
//! messages in the cloud share. Each connection gets its own thread; the
//! handheld connects only around a game's launch and exit or when the person
//! starts a sync, so this stays idle almost all the time.

use std::net::{TcpListener, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::discovery;
use droidtop_agent_core::keys::{short, PeerId};
use droidtop_agent_core::server::{serve, serve_moved};
use droidtop_agent_core::tunnel;
use droidtop_agent_core::PORT;

use crate::state::Agent;

pub fn run(agent: Arc<Agent>) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", PORT))?;
    println!("{} ({}) is listening on port {PORT}.", agent.name(), short(&agent.peer_id()));
    let stop = Arc::new(AtomicBool::new(false));

    match UdpSocket::bind(("0.0.0.0", PORT)) {
        Ok(sock) => {
            let a = agent.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                let name = a.name();
                if let Err(e) = discovery::answer_agent_queries(&sock, &a.key, &name, PORT, |p| a.is_trusted(p), &stop) {
                    eprintln!("Discovery stopped: {e}");
                }
            });
        }
        Err(e) => eprintln!("Handhelds will not find this computer by broadcast ({e}); they can still use its address."),
    }

    {
        let a = agent.clone();
        thread::spawn(move || loop {
            let changes = a.rescan();
            if changes > 0 {
                println!("Library: {changes} changes from the scan.");
            }
            let minutes = a.settings.lock().unwrap().scan_minutes.max(5);
            thread::sleep(Duration::from_secs(minutes * 60));
        });
    }

    {
        let a = agent.clone();
        thread::spawn(move || loop {
            if let Err(e) = crate::share::process(&a) {
                eprintln!("Cloud share: {e}");
            }
            thread::sleep(Duration::from_secs(60));
        });
    }

    // Direct WireGuard for a paired handheld away from the LAN: dual-stack
    // where the system allows it, and IPv4 beside it where it does not
    // (Windows binds [::] to IPv6 only).
    let sockets: Vec<UdpSocket> =
        ["[::]", "0.0.0.0"].iter().filter_map(|b| UdpSocket::bind(format!("{b}:{}", tunnel::WG_PORT)).ok()).collect();
    // STUN and hole punching need the socket that speaks IPv4: the IPv4 one
    // where there are two (Windows), else the dual-stack one.
    let rendezvous_on = sockets.iter().rposition(|s| s.local_addr().is_ok());
    if rendezvous_on.is_some() {
        crate::rendezvous::run(agent.clone());
    }
    for (i, udp) in sockets.into_iter().enumerate() {
        let a = agent.clone();
        let stop = stop.clone();
        let mut hooks: Box<dyn tunnel::Hooks + Send> = if Some(i) == rendezvous_on {
            Box::new(crate::rendezvous::WgHooks::new(agent.rendezvous.clone()))
        } else {
            Box::new(tunnel::NoHooks)
        };
        thread::spawn(move || {
            let peers = || -> Vec<PeerId> {
                a.devices.lock().unwrap().iter().filter_map(|d| PeerId::from_hex(&d.id).ok()).filter(|p| a.is_trusted(p)).collect()
            };
            let on_stream = |peer: PeerId, mut stream: tunnel::TunnelStream| {
                let a = a.clone();
                thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(120)));
                    match Channel::accept(stream, &a.key, |p| *p == peer && a.is_trusted(p)) {
                        Ok(mut ch) => {
                            a.seen(&peer, None);
                            if let Err(e) = serve(&mut ch, &*a) {
                                eprintln!("Session with {} through WireGuard ended: {e}", short(&peer));
                            }
                        }
                        Err(e) => eprintln!("Refused a WireGuard session from {}: {e}", short(&peer)),
                    }
                });
            };
            if let Err(e) = tunnel::serve(&a.key, udp, peers, on_stream, &stop, hooks.as_mut()) {
                eprintln!("WireGuard stopped: {e}");
            }
        });
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let a = agent.clone();
        thread::spawn(move || {
            let address = stream.peer_addr().ok().map(|p| p.ip().to_string());
            let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));
            // While this computer moves to the identity it shares with
            // windowcast, it answers to its previous one too and tells the
            // handheld (crate::state::migrate).
            let keys: Vec<&droidtop_agent_core::keys::DeviceKey> = std::iter::once(&a.key).chain(a.previous.as_ref()).collect();
            let (mut ch, which) = match Channel::accept_any(stream, &keys, |p| a.is_trusted(p)) {
                Ok(accepted) => accepted,
                Err(e) => {
                    eprintln!("Refused a connection from {}: {e}", address.unwrap_or_default());
                    return;
                }
            };
            let peer = ch.peer();
            a.seen(&peer, address);
            let served = if which == 0 {
                a.on_current_key(&peer);
                serve(&mut ch, &*a)
            } else {
                serve_moved(&mut ch, &*a, a.moved())
            };
            if let Err(e) = served {
                eprintln!("Session with {} ended: {e}", short(&peer));
            }
        });
    }
    Ok(())
}
