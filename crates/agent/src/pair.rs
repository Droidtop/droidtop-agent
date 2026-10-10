//! Pairing, from the computer.
//! - `droidtop-agent pair <code>`: the handheld shows the code (Settings >
//!   Computers > Pair a computer on droidtop) and this computer connects to
//!   it, found on the LAN or at the address in the QR code's text.
//! - `droidtop-agent pair`: this computer shows its addresses and a code, and
//!   the handheld connects (Pair a computer > "Use a code from the computer").
//!   For a handheld this computer cannot reach: one behind an emulator's or a
//!   guest network's NAT.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use droidtop_agent_core::pairing::{new_code, pair_client, pair_host, PairInvite};
use droidtop_agent_core::{discovery, PAIR_PORT};

use crate::state::Agent;

/// How long the computer shows a code.
const SHOW_FOR: Duration = Duration::from_secs(10 * 60);

/// Wrong codes accepted before the code is dropped.
const MAX_ATTEMPTS: u32 = 3;

/// This computer showing a code: the port it listens on, its addresses and
/// the code, until a handheld pairs or the time runs out.
pub struct Showing {
    listener: TcpListener,
    pub port: u16,
    pub code: String,
    /// `ip:port` for each of this computer's IPv4 addresses.
    pub addresses: Vec<String>,
    pub until: Instant,
}

impl Showing {
    /// The invitation as text, with this computer's first address: what a
    /// QR code of it carries.
    pub fn invite(&self, agent: &Agent) -> String {
        PairInvite { code: self.code.clone(), peer: Some(agent.peer_id()), name: Some(agent.name()), at: self.addresses.first().cloned() }
            .to_uri()
    }
}

/// Opens the pairing port and makes a new code.
pub fn listen() -> Result<Showing, String> {
    let listener = TcpListener::bind(("0.0.0.0", PAIR_PORT))
        .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
        .map_err(|e| format!("Could not open a port for pairing: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let addresses: Vec<String> = if_addrs::get_if_addrs()
        .map(|all| all.into_iter().filter(|i| !i.is_loopback() && i.ip().is_ipv4()).map(|i| format!("{}:{port}", i.ip())).collect())
        .unwrap_or_default();
    Ok(Showing { listener, port, code: new_code(), addresses, until: Instant::now() + SHOW_FOR })
}

/// Shows this computer's addresses and a new code on the console, and waits for a handheld.
pub fn show(agent: &Agent) -> Result<String, String> {
    let showing = listen()?;
    let (port, code) = (showing.port, &showing.code);
    println!("On droidtop: Settings > Computers > Pair a computer > Use a code from the computer.");
    let addresses = &showing.addresses;
    println!("Address: {}", if addresses.is_empty() { format!("this computer's address, port {port}") } else { addresses.join("  or  ") });
    println!("Code:    {} {}", &code[..3], &code[3..]);
    println!("(An Android emulator reaches this computer at 10.0.2.2:{port}.) Waiting up to 10 minutes; Ctrl+C stops.");
    wait(agent, showing, &AtomicBool::new(false))
}

/// Waits for a handheld to pair with the code [`showing`] shows, until it
/// pairs, three codes were wrong, the time runs out or [`cancel`] is set.
pub fn wait(agent: &Agent, showing: Showing, cancel: &AtomicBool) -> Result<String, String> {
    let Showing { listener, code, until: deadline, .. } = showing;
    let mut attempts = 0;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err("Pairing stopped.".into());
        }
        match listener.accept() {
            Ok((mut stream, from)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
                match pair_host(&mut stream, &agent.key, &agent.name(), &code) {
                    Ok(paired) => {
                        agent
                            .add_device(paired.peer, &paired.name)
                            .map_err(|e| format!("Paired, but the pairing could not be saved: {e}"))?;
                        return Ok(paired.name);
                    }
                    Err(e) => {
                        attempts += 1;
                        eprintln!("A pairing attempt from {} failed: {e}", from.ip());
                        if attempts >= MAX_ATTEMPTS {
                            return Err(format!("Stopped after {MAX_ATTEMPTS} wrong codes. Run droidtop-agent pair again for a new one."));
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("No handheld paired in time. Run droidtop-agent pair again for a new code.".into())
}

pub fn pair(agent: &Agent, input: &str) -> Result<String, String> {
    let invite =
        PairInvite::parse(input).ok_or("That is not a pairing code: type the 6 digits the handheld shows, or the text of its QR code.")?;
    let address: SocketAddr = match &invite.at {
        Some(at) => at.parse().map_err(|_| format!("The QR code's address {at} is not valid."))?,
        None => {
            let found = discovery::find_pairing(Duration::from_secs(3))
                .map_err(|e| format!("Could not look for the handheld on this network: {e}"))?;
            let wanted: Vec<_> = match &invite.peer {
                Some(peer) => found.into_iter().filter(|(a, _)| a.id == peer.to_hex()).collect(),
                None => found,
            };
            match wanted.as_slice() {
                [] => return Err("No handheld is showing a pairing code on this network. On droidtop open Settings > Computers > Pair a computer, keep it open, and try again; or type the text of its QR code here.".into()),
                [(_, addr)] => *addr,
                many => {
                    let names: Vec<String> = many.iter().map(|(a, addr)| format!("{} at {addr}", a.name)).collect();
                    return Err(format!("More than one handheld is showing a code ({}). Use the text of the QR code to pick one.", names.join(", ")));
                }
            }
        }
    };
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|e| format!("Could not reach the handheld at {address}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let paired = pair_client(&mut stream, &agent.key, &agent.name(), &invite.code)
        .map_err(|e| format!("Pairing did not finish: {e}. Check the code and try again."))?;
    if invite.peer.is_some_and(|p| p != paired.peer) {
        return Err("The handheld that answered is not the one in the QR code; nothing was paired.".into());
    }
    agent.add_device(paired.peer, &paired.name).map_err(|e| format!("Paired, but the pairing could not be saved: {e}"))?;
    Ok(paired.name)
}
