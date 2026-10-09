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
use droidtop_agent_core::keys::short;
use droidtop_agent_core::server::serve;
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

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let a = agent.clone();
        thread::spawn(move || {
            let address = stream.peer_addr().ok().map(|p| p.ip().to_string());
            let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));
            let mut ch = match Channel::accept(stream, &a.key, |p| a.is_trusted(p)) {
                Ok(ch) => ch,
                Err(e) => {
                    eprintln!("Refused a connection from {}: {e}", address.unwrap_or_default());
                    return;
                }
            };
            let peer = ch.peer();
            a.seen(&peer, address);
            if let Err(e) = serve(&mut ch, &*a) {
                eprintln!("Session with {} ended: {e}", short(&peer));
            }
        });
    }
    Ok(())
}
