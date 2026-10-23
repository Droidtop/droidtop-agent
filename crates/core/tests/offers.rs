//! What a device offers another (docs/DESIGN.md section 20), over loopback:
//! a request the host does not offer is answered `not_offered` and nothing
//! else, a save file sent to a host that does not offer saves is read and
//! refused without breaking the session, the hello states what is offered,
//! and a change to the offer holds at the next request.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;

use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::Change;
use droidtop_agent_core::manifest::FileEntry;
use droidtop_agent_core::proto::{GameRef, Offer, Request, Response, NOT_OFFERED};
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_core::server::{serve, Host};
use droidtop_agent_core::Result;

struct Pc {
    offer: Mutex<Offer>,
}

impl Host for Pc {
    fn name(&self) -> String {
        "Desk".into()
    }
    fn offers(&self, _peer: &PeerId) -> Offer {
        *self.offer.lock().unwrap()
    }
    fn syncs_contexts(&self, _peer: &PeerId) -> bool {
        false
    }
    fn saves(&self, _game: &GameRef) -> Option<(SaveSpec, Roots)> {
        None
    }
    fn archive_dir(&self, _game: &GameRef) -> PathBuf {
        std::env::temp_dir()
    }
    fn library_pull(&self, _peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)> {
        Ok((Vec::new(), since))
    }
    fn library_push(&self, _peer: &PeerId, _changes: Vec<Change>) -> Result<()> {
        Ok(())
    }
    fn context_pull(&self, _context: &str, _offer: Option<&AdapterOffer>) -> Result<Records> {
        panic!("a context is never pulled from a device that does not sync them")
    }
    fn context_push(&self, _context: &str, _changes: Vec<RecordChange>) -> Result<Option<String>> {
        panic!("a context is never pushed to a device that does not sync them")
    }
}

fn session(host: &'static Pc, client: impl FnOnce(&mut Channel<TcpStream>)) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (pc, other) = (DeviceKey::generate(), DeviceKey::generate());
    let (pc_id, other_id) = (pc.peer_id(), other.peer_id());
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut ch = Channel::accept(s, &pc, |p| *p == other_id).unwrap();
        let _ = serve(&mut ch, host);
    });
    let mut ch = Channel::connect(TcpStream::connect(addr).unwrap(), &other, &pc_id).unwrap();
    client(&mut ch);
    ch.send_json(&Request::Bye).unwrap();
    server.join().unwrap();
}

fn ask(ch: &mut Channel<TcpStream>, request: &Request) -> Response {
    ch.send_json(request).unwrap();
    ch.recv_json().unwrap()
}

fn not_offered(reply: &Response) -> bool {
    matches!(reply, Response::Error { code: Some(code), .. } if code == NOT_OFFERED)
}

fn features(ch: &mut Channel<TcpStream>) -> Vec<String> {
    match ask(ch, &Request::Hello { name: "Lap".into(), version: 1, features: Vec::new(), disco: None }) {
        Response::Hello { features, .. } => features,
        other => panic!("{other:?}"),
    }
}

#[test]
fn nothing_offered_is_refused_and_says_nothing_more() {
    let host: &'static Pc = Box::leak(Box::new(Pc { offer: Mutex::new(Offer::NONE) }));
    session(host, |ch| {
        let hello = features(ch);
        assert!(!hello.iter().any(|f| f.starts_with("offers_")), "{hello:?}");
        let game = GameRef { key: "steam:440".into(), title: "Team Fortress 2".into() };
        for request in [
            Request::LibraryPull { since: 0 },
            Request::GameFiles { game: game.clone() },
            Request::SaveSpec { game: game.clone() },
            Request::SaveManifest { game: game.clone() },
            Request::ContextPull { context: "f95checker".into(), adapter: None },
        ] {
            let reply = ask(ch, &request);
            assert!(not_offered(&reply), "{request:?}: {reply:?}");
            if let Response::Error { message, .. } = reply {
                assert!(message.starts_with("Desk does not offer"), "{message}");
            }
        }
        // The file's contents follow its header; they are read, the put is
        // refused, and the session goes on.
        let contents = vec![7u8; 3000];
        let file = FileEntry { name: "save.dat".into(), size: contents.len() as u64, mtime_ms: 0, sha256: String::new() };
        ch.send_json(&Request::FilePut { game: game.clone(), file }).unwrap();
        ch.send(&contents).unwrap();
        assert!(not_offered(&ch.recv_json().unwrap()));
        // What the other device gives of its own is accepted.
        assert!(matches!(ask(ch, &Request::LibraryPush { changes: Vec::new() }), Response::Ok));
    });
}

#[test]
fn an_offer_is_stated_in_the_hello_and_holds_at_the_next_request() {
    let host: &'static Pc = Box::leak(Box::new(Pc { offer: Mutex::new(Offer::NONE) }));
    session(host, |ch| {
        assert!(not_offered(&ask(ch, &Request::LibraryPull { since: 0 })));
        *host.offer.lock().unwrap() = Offer { library: true, saves: false };
        assert!(matches!(ask(ch, &Request::LibraryPull { since: 0 }), Response::LibraryChanges { .. }));
        let game = GameRef { key: "steam:440".into(), title: "Team Fortress 2".into() };
        assert!(not_offered(&ask(ch, &Request::SaveSpec { game: game.clone() })));
        let hello = features(ch);
        assert!(hello.contains(&"offers_library".to_string()) && !hello.contains(&"offers_saves".to_string()), "{hello:?}");
        *host.offer.lock().unwrap() = Offer::LIBRARY_AND_SAVES;
        assert!(matches!(ask(ch, &Request::SaveSpec { game }), Response::SaveSpec { .. }));
        assert!(features(ch).contains(&"offers_saves".to_string()));
    });
}
