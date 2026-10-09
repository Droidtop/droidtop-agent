//! `droidtop-agent pair <code>`: pairs this computer with a handheld showing
//! a pairing code (Settings > Computers > Pair a computer on droidtop). The
//! handheld is found on the LAN, or at the address in the QR code's text.

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use droidtop_agent_core::discovery;
use droidtop_agent_core::pairing::{pair_client, PairInvite};

use crate::state::Agent;

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
