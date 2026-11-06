//! The computer's side of a droidtop-run relay (docs/DESIGN.md section 10, transport 4;
//! Droidtop/tracker#800): when one is set (`droidtop-agent relay set <host:port>`), the running agent
//! keeps a connection to it, takes packets only from its paired devices, and hands each call that
//! arrives to its own WireGuard socket on loopback. The relay forwards the tunnel's packets and
//! nothing else, so what a handheld does through it is a normal WireGuard call.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::tunnel;
use droidtop_agent_relay::{Bridge, RELAY_PORT};
use droidtop_agent_store::{read_json, write_json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Agent;

/// What `relay.json` beside the agent's settings holds.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct RelaySettings {
    /// The relay's `host:port` (the port defaults to [`RELAY_PORT`]); none when no relay is used.
    #[serde(default)]
    pub address: Option<String>,
}

fn path(agent: &Agent) -> PathBuf {
    agent.store.dirs.config.join("relay.json")
}

pub fn settings(agent: &Agent) -> RelaySettings {
    read_json(&path(agent)).unwrap_or_default()
}

/// Sets the relay, or none.
pub fn set(agent: &Agent, address: Option<&str>) -> io::Result<()> {
    let address = address.map(|a| if a.contains(':') && !a.ends_with(']') { a.to_string() } else { format!("{a}:{RELAY_PORT}") });
    write_json(&path(agent), &RelaySettings { address })
}

fn paired(agent: &Agent) -> Vec<PeerId> {
    agent.store.devices_now().iter().filter_map(|d| PeerId::from_hex(&d.id).ok()).filter(|p| agent.store.is_trusted(p)).collect()
}

/// Keeps the agent on its relay while it runs: connects when one is set, tells the relay when the
/// paired devices change, and connects again a minute after the connection ends.
pub fn run(agent: Arc<Agent>) {
    thread::spawn(move || {
        let wireguard = SocketAddr::from((Ipv4Addr::LOCALHOST, tunnel::WG_PORT));
        let mut bridge: Option<(String, Bridge, Vec<PeerId>)> = None;
        let mut retry_at = Instant::now();
        while !agent.stop.is_stopped() {
            let wanted = settings(&agent).address;
            // A changed or cleared setting, or a connection that ended, drops the bridge.
            if bridge.as_ref().is_some_and(|(at, b, _)| Some(at) != wanted.as_ref() || !b.is_alive()) {
                if let Some((at, _, _)) = bridge.take() {
                    agent.events.info("relay", format!("Relay {at}: disconnected."), json!({ "relay": at }));
                }
                retry_at = Instant::now() + Duration::from_secs(60);
            }
            match (&mut bridge, wanted) {
                (Some((_, b, allowed)), Some(_)) => {
                    let now = paired(&agent);
                    if *allowed != now && b.set_allowed(now.clone()).is_ok() {
                        *allowed = now;
                    }
                }
                (None, Some(at)) if Instant::now() >= retry_at => {
                    let allowed = paired(&agent);
                    match Bridge::connect(at.as_str(), &agent.store.key, allowed.clone(), Some(wireguard), Duration::from_secs(10)) {
                        Ok(b) => {
                            agent.events.info(
                                "relay",
                                format!("Relay {at}: connected, as a fallback for paired devices."),
                                json!({ "relay": at }),
                            );
                            bridge = Some((at, b, allowed));
                        }
                        Err(e) => {
                            agent.events.warn("relay", format!("Relay {at}: {e}"), json!({ "relay": at }));
                            retry_at = Instant::now() + Duration::from_secs(60);
                        }
                    }
                }
                _ => {}
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}
