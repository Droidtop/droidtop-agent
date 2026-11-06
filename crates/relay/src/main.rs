//! `droidtop-relay [<listen address>]`: the relay for the server VM (docs/DESIGN.md section 10,
//! transport 4; Droidtop/tracker#364). It listens on TCP 47615 on every address unless told
//! otherwise, keeps nothing on disk and holds no key.

use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use droidtop_agent_relay::{serve, Relay, RELAY_PORT};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let listen = match args.as_slice() {
        [] => format!("[::]:{RELAY_PORT}"),
        [addr] if addr != "--help" && addr != "-h" => addr.clone(),
        _ => {
            eprintln!("usage: droidtop-relay [<listen address>]   (default [::]:{RELAY_PORT})");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(&listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("droidtop-relay: cannot listen on {listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("droidtop-relay: listening on {listen}");
    let stop = AtomicBool::new(false);
    match serve(&Relay::new(), listener, &stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("droidtop-relay: {e}");
            ExitCode::FAILURE
        }
    }
}
