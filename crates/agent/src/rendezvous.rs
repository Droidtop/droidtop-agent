//! The computer's side of rendezvous away from the LAN (docs/DESIGN.md
//! section 10): Syncthing's discovery and routing, as its own client does it.
//! - STUN keepalives from the WireGuard socket itself ([`WgHooks`]), so the
//!   NAT keeps one mapping for UDP 47611 and the agent knows its address.
//! - An announcement of that address (and any forwarded port or global IPv6
//!   address) to global discovery under this computer's discovery ID: when it
//!   changes, then when the server says (30 minutes by default).
//! - Lookups of the paired handhelds' discovery IDs on Syncthing's schedule
//!   (a found address is kept 5 minutes; not found, asked again after a
//!   minute or the server's `Retry-After`), and a tiny punch packet every 2
//!   seconds towards each address found, so the handheld's WireGuard
//!   handshakes get through this side's NAT.
//!
//! Only addresses go to the discovery servers. Syncthing's relays are never
//! used: every byte of a sync goes through the direct WireGuard tunnel.

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::rendezvous::{self, Answer, Server, StunKeeper};
use droidtop_agent_core::tunnel::Hooks;
use serde::{Deserialize, Serialize};

use crate::state::Agent;

/// What the WireGuard loop and the announcer share.
#[derive(Default)]
pub struct Shared {
    /// The STUN servers, resolved off the loop.
    pub stun: Vec<SocketAddr>,
    /// The address the NAT gives the WireGuard socket.
    pub mapped: Option<SocketAddr>,
    /// Where to punch, and until when.
    pub punch: Vec<(SocketAddr, Instant)>,
}

/// How often a punch packet goes to each address found.
const PUNCH_EVERY: Duration = Duration::from_secs(2);

/// STUN and punching on the WireGuard socket.
pub struct WgHooks {
    shared: Arc<Mutex<Shared>>,
    keeper: StunKeeper,
    last_punch: Option<Instant>,
}

impl WgHooks {
    pub fn new(shared: Arc<Mutex<Shared>>) -> WgHooks {
        WgHooks { shared, keeper: StunKeeper::new(Vec::new()), last_punch: None }
    }
}

impl Hooks for WgHooks {
    fn datagram(&mut self, _udp: &UdpSocket, _from: SocketAddr, data: &[u8]) -> bool {
        if !self.keeper.heard(data) {
            return false;
        }
        if let Some(mapped) = self.keeper.mapped {
            self.shared.lock().unwrap().mapped = Some(mapped);
        }
        true
    }

    fn tick(&mut self, udp: &UdpSocket) {
        if self.keeper.servers.is_empty() {
            let stun = self.shared.lock().unwrap().stun.clone();
            if stun.is_empty() {
                return;
            }
            self.keeper.servers = stun;
        }
        self.keeper.tick(udp);
        if self.last_punch.is_some_and(|t| t.elapsed() < PUNCH_EVERY) {
            return;
        }
        self.last_punch = Some(Instant::now());
        let v6 = udp.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
        let mut shared = self.shared.lock().unwrap();
        shared.punch.retain(|(_, until)| *until > Instant::now());
        for (to, _) in &shared.punch {
            // One byte: opens this side's NAT towards the handheld, and
            // WireGuard there drops it as noise.
            let _ = udp.send_to(&[0], if v6 { rendezvous::to_v6(*to) } else { *to });
        }
    }
}

/// What `droidtop-agent rendezvous` shows: kept in a small file by the running agent.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Status {
    pub disco: Option<String>,
    pub mapped: Option<String>,
    pub announced: Vec<String>,
    pub announced_at_ms: i64,
    pub next_announce_ms: i64,
    pub last_problem: Option<String>,
    /// By handheld name: the address last found for it.
    pub found: Vec<(String, String)>,
}

/// Announces and looks up for as long as the agent runs. Blocking work
/// (names, https) happens here, never in the WireGuard loop.
pub fn run(agent: Arc<Agent>) {
    thread::spawn(move || {
        let Ok(cert) = rendezvous::certificate(&agent.key) else {
            eprintln!("Rendezvous is off: this computer's discovery certificate could not be made.");
            return;
        };
        let mut status = Status { disco: Some(cert.device_id()), ..Default::default() };
        let mut stun_resolved: Option<Instant> = None;
        let mut announce_at = Instant::now() + Duration::from_secs(5);
        let mut announced: Vec<String> = Vec::new();
        // Never before a server's Retry-After, or 5 minutes after a failure.
        let mut blocked_until = Instant::now();
        let mut lookup_at: std::collections::BTreeMap<String, Instant> = Default::default();
        loop {
            thread::sleep(Duration::from_secs(5));
            let (on, servers, stun) = {
                let s = agent.settings.lock().unwrap();
                (s.rendezvous, Server::list(&s.discovery_servers), s.stun_servers.clone())
            };
            if !on {
                agent.rendezvous.lock().unwrap().stun.clear();
                continue;
            }
            if stun_resolved.is_none_or(|t| t.elapsed() > Duration::from_secs(3600)) {
                let resolved = rendezvous::resolve_stun(&stun);
                stun_resolved = Some(Instant::now());
                agent.rendezvous.lock().unwrap().stun = resolved;
            }
            // Announce what this computer can be reached at, when it changed
            // (after a 5 s settle, as Syncthing does) or when the server said.
            let addresses = addresses(&agent);
            if !addresses.is_empty() && (addresses != announced || Instant::now() >= announce_at) {
                if addresses != announced && Instant::now() + Duration::from_secs(5) < announce_at {
                    announce_at = (Instant::now() + Duration::from_secs(5)).max(blocked_until);
                }
                if Instant::now() >= announce_at {
                    match rendezvous::announce(&servers, &cert, &addresses) {
                        Answer::Announced(after) => {
                            announced = addresses.clone();
                            announce_at = Instant::now() + after;
                            status.announced = addresses;
                            status.announced_at_ms = droidtop_agent_core::library::now_ms();
                            status.next_announce_ms = status.announced_at_ms + after.as_millis() as i64;
                            status.last_problem = None;
                        }
                        Answer::Wait(after) => {
                            announce_at = Instant::now() + after;
                            blocked_until = announce_at;
                            status.last_problem = Some(format!("announcing failed; trying again in {} s", after.as_secs()));
                        }
                        Answer::Found(_) => {}
                    }
                }
            }
            // Look the paired handhelds up on Syncthing's schedule.
            let devices: Vec<(String, String)> =
                agent.devices.lock().unwrap().iter().filter_map(|d| d.disco.clone().map(|id| (d.name.clone(), id))).collect();
            for (name, id) in devices {
                if lookup_at.get(&id).is_some_and(|t| Instant::now() < *t) {
                    continue;
                }
                match rendezvous::lookup(&servers, &id) {
                    Answer::Found(found) => {
                        lookup_at.insert(id.clone(), Instant::now() + rendezvous::FOUND_CACHE);
                        let until = Instant::now() + rendezvous::FOUND_CACHE;
                        let mut shared = agent.rendezvous.lock().unwrap();
                        for to in rendezvous::wg_endpoints(&found).into_iter().filter(SocketAddr::is_ipv4) {
                            shared.punch.retain(|(a, _)| *a != to);
                            shared.punch.push((to, until));
                            status.found.retain(|(n, _)| *n != name);
                            status.found.push((name.clone(), to.to_string()));
                        }
                    }
                    Answer::Wait(after) | Answer::Announced(after) => {
                        lookup_at.insert(id.clone(), Instant::now() + after.max(rendezvous::NOT_FOUND_CACHE));
                    }
                }
            }
            status.mapped = agent.rendezvous.lock().unwrap().mapped.map(|m| m.to_string());
            let _ = crate::state::write_json(&agent.dirs.rendezvous(), &status);
        }
    });
}

/// What this computer announces: the address STUN found for its WireGuard
/// socket, and the endpoints it states in hello (a forwarded port, global
/// IPv6 addresses), as `wg://` addresses.
fn addresses(agent: &Agent) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(mapped) = agent.rendezvous.lock().unwrap().mapped {
        out.push(format!("{}{mapped}", rendezvous::SCHEME));
    }
    let public = agent.settings.lock().unwrap().public_endpoint.clone();
    for e in crate::host::endpoints(public.as_deref()) {
        if let Some(a) = e.strip_prefix("wg:") {
            out.push(format!("{}{a}", rendezvous::SCHEME));
        }
    }
    out.sort();
    out.dedup();
    out
}
