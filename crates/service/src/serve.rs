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
use serde_json::json;

use crate::Agent;

pub fn run(agent: Arc<Agent>) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", PORT))?;
    agent.events.info(
        "listening",
        format!("{} ({}) is listening on port {PORT}.", agent.store.name(), short(&agent.store.peer_id())),
        json!({ "name": agent.store.name(), "id": agent.store.peer_id().to_hex(), "port": PORT }),
    );
    let stop = Arc::new(AtomicBool::new(false));

    match UdpSocket::bind(("0.0.0.0", PORT)) {
        Ok(sock) => {
            let a = agent.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                let name = a.store.name();
                if let Err(e) = discovery::answer_agent_queries(&sock, &a.store.key, &name, PORT, |p| a.store.is_trusted(p), &stop) {
                    a.events.warn("discovery", format!("Discovery stopped: {e}"), json!({}));
                }
            });
        }
        Err(e) => agent.events.warn(
            "discovery",
            format!("Handhelds will not find this computer by broadcast ({e}); they can still use its address."),
            json!({}),
        ),
    }

    {
        let a = agent.clone();
        thread::spawn(move || loop {
            let changes = a.rescan();
            if changes > 0 {
                a.events.info("library", format!("Library: {changes} changes from the scan."), json!({ "changes": changes }));
            }
            let minutes = a.store.settings.lock().unwrap().scan_minutes.max(5);
            thread::sleep(Duration::from_secs(minutes * 60));
        });
    }

    {
        let a = agent.clone();
        thread::spawn(move || loop {
            if let Err(e) = crate::share::process(&a) {
                a.events.warn("share", format!("Cloud share: {e}"), json!({}));
            }
            thread::sleep(Duration::from_secs(60));
        });
    }

    {
        let a = agent.clone();
        thread::spawn(move || loop {
            crate::syncthing::tick(&a);
            thread::sleep(Duration::from_secs(30));
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
                a.store.devices_now().iter().filter_map(|d| PeerId::from_hex(&d.id).ok()).filter(|p| a.store.is_trusted(p)).collect()
            };
            let on_stream = |peer: PeerId, mut stream: tunnel::TunnelStream| {
                let a = a.clone();
                thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(120)));
                    match Channel::accept(stream, &a.store.key, |p| *p == peer && a.store.is_trusted(p)) {
                        Ok(mut ch) => {
                            a.store.seen(&peer, None);
                            let served = serve(&mut ch, &*a);
                            session_ended(&a, &peer, " through WireGuard", served);
                        }
                        Err(e) => a.events.warn(
                            "session",
                            format!("Refused a WireGuard session from {}: {e}", short(&peer)),
                            json!({ "peer": peer.to_hex() }),
                        ),
                    }
                });
            };
            if let Err(e) = tunnel::serve(&a.store.key, udp, peers, on_stream, &stop, hooks.as_mut()) {
                a.events.warn("wireguard", format!("WireGuard stopped: {e}"), json!({}));
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
            // handheld (droidtop_agent_store::migrate).
            let keys: Vec<&droidtop_agent_core::keys::DeviceKey> = std::iter::once(&a.store.key).chain(a.store.previous.as_ref()).collect();
            let (mut ch, which) = match Channel::accept_any(stream, &keys, |p| a.store.is_trusted(p)) {
                Ok(accepted) => accepted,
                Err(e) => {
                    let address = address.unwrap_or_default();
                    a.events.warn("session", format!("Refused a connection from {address}: {e}"), json!({ "address": address }));
                    return;
                }
            };
            let peer = ch.peer();
            a.store.seen(&peer, address);
            let served = if which == 0 {
                a.store.on_current_key(&peer);
                serve(&mut ch, &*a)
            } else {
                serve_moved(&mut ch, &*a, a.store.moved())
            };
            session_ended(&a, &peer, "", served);
        });
    }
    Ok(())
}

/// A session with [`peer`] is over: said as a warning when it broke off.
/// [`via`] names the transport when it was not the LAN.
fn session_ended(agent: &Agent, peer: &PeerId, via: &str, served: droidtop_agent_core::Result<()>) {
    let device = agent.store.device_name(peer);
    let detail = |error: Option<String>| json!({ "peer": peer.to_hex(), "device": device, "error": error });
    match served {
        Ok(()) => agent.events.info("session", format!("Session with {device}{via} ended."), detail(None)),
        Err(e) => agent.events.warn("session", format!("Session with {device}{via} ended: {e}"), detail(Some(e.to_string()))),
    }
}
