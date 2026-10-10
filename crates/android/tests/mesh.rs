//! Many handhelds and many computers, paired in a partial mesh (owner,
//! 2026-10-10: "if we have 5 droidtop devices and 5 desktops, we should be
//! able to connect and pair any number of permutations of these devices and
//! have them work"). Three handhelds call the same JSON surface droidtop
//! calls; three computers serve the core's protocol over loopback, each
//! taking only the handhelds paired with it:
//!
//! ```text
//!   H1 ── C1      H1 ── C2 ── H2 ── C3 ── H3
//! ```
//!
//! It covers the library relaying across the mesh (one entry per game, with
//! an install per device, and the person's organization of it), saves edited on two devices at once (the newest
//! copy wins and the other device keeps its own), a preferred computer
//! beating a newer copy, and saves reaching a handheld through a computer it
//! shares with another.

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use droidtop_agent::api::call;
use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::{Change, Install, Library, ScannedGame};
use droidtop_agent_core::moved::Moved;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_core::savesync::copy_dir;
use droidtop_agent_core::server::{serve, serve_moved, Host};
use serde_json::{json, Value};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dta-mesh-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A computer: its saves under its own folder, its library, its name.
struct Computer {
    name: String,
    dir: PathBuf,
    library: Mutex<Library>,
}

impl Computer {
    fn appdata(&self) -> PathBuf {
        self.dir.join("AppData/Roaming")
    }
    fn save(&self) -> PathBuf {
        self.appdata().join("Game/slot.sav")
    }
}

impl Host for Computer {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)> {
        (game.key == "steam:1")
            .then(|| (SaveSpec { patterns: vec!["<winAppData>/Game".into()] }, Roots::from([("<winAppData>".into(), self.appdata())])))
    }
    fn archive_dir(&self, _game: &GameRef) -> PathBuf {
        self.dir.join("archive")
    }
    fn library_pull(&self, _peer: &PeerId, since: u64) -> droidtop_agent_core::Result<(Vec<Change>, u64)> {
        Ok(self.library.lock().unwrap().changes_since(since))
    }
    fn library_push(&self, _peer: &PeerId, changes: Vec<Change>) -> droidtop_agent_core::Result<()> {
        let mut lib = self.library.lock().unwrap();
        changes.into_iter().for_each(|c| {
            lib.apply(c);
        });
        Ok(())
    }
    fn context_pull(&self, _context: &str, _offer: Option<&AdapterOffer>) -> droidtop_agent_core::Result<Records> {
        Ok(Records::new())
    }
    fn context_push(&self, _context: &str, _changes: Vec<RecordChange>) -> droidtop_agent_core::Result<Option<String>> {
        Ok(None)
    }
}

/// Starts [`computer`] serving the handhelds in [`paired`]; returns its id and address.
fn start(name: &str, games: &[&str], paired: Vec<PeerId>) -> (&'static Computer, PeerId, SocketAddr) {
    let key = DeviceKey::generate();
    let id = key.peer_id();
    let mut library = Library::default();
    let scanned = games
        .iter()
        .map(|g| ScannedGame {
            key: g.to_string(),
            title: g.to_string(),
            platform: Some("pc".into()),
            install: Install { installed: true, ..Default::default() },
        })
        .collect();
    library.update_device(&id, name, scanned);
    let computer: &'static Computer = Box::leak(Box::new(Computer { name: name.into(), dir: temp(name), library: Mutex::new(library) }));
    fs::create_dir_all(computer.appdata().join("Game")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (key, paired) = (DeviceKey::from_seed(&key.seed()).unwrap(), paired.clone());
            thread::spawn(move || {
                if let Ok(mut ch) = Channel::accept(stream, &key, |p| paired.contains(p)) {
                    let _ = serve(&mut ch, computer);
                }
            });
        }
    });
    (computer, id, at)
}

/// A handheld: its key, its saves folder and its library file.
struct Handheld {
    name: &'static str,
    seed: Value,
    id: PeerId,
    dir: PathBuf,
}

impl Handheld {
    fn new(name: &'static str) -> Handheld {
        let made: Value = serde_json::from_str(&call("identity_new", "{}")).unwrap();
        Handheld { name, id: PeerId::from_hex(made["id"].as_str().unwrap()).unwrap(), seed: made["seed"].clone(), dir: temp(name) }
    }
    fn appdata(&self) -> PathBuf {
        self.dir.join("AppData/Roaming")
    }
    fn save(&self) -> PathBuf {
        self.appdata().join("Game/slot.sav")
    }
    fn args(&self, pc: &(&'static Computer, PeerId, SocketAddr)) -> Value {
        json!({ "seed": self.seed, "peer": pc.1.to_hex(), "addresses": [pc.2.to_string()], "name": self.name })
    }
    fn library(&self, pc: &(&'static Computer, PeerId, SocketAddr)) -> Value {
        self.library_with(pc, json!({}))
    }
    /// A library exchange reporting [`marks`], the person's organization of
    /// games as droidtop keeps it.
    fn library_with(&self, pc: &(&'static Computer, PeerId, SocketAddr), marks: Value) -> Value {
        let mut args = self.args(pc);
        args["state"] = json!(self.dir.join("library.json"));
        args["scan"] = json!([]);
        args["marks"] = marks;
        let out: Value = serde_json::from_str(&call("sync_library", &args.to_string())).unwrap();
        assert!(out.get("error").is_none() && out.get("unreachable").is_none(), "{} with {}: {out}", self.name, pc.0.name);
        out
    }
    fn saves(&self, pc: &(&'static Computer, PeerId, SocketAddr), primary: bool) -> Value {
        let mut args = self.args(pc);
        args["game"] = json!({ "key": "steam:1", "title": "Game" });
        args["roots"] = json!({ "<winAppData>": self.appdata() });
        args["baseline"] = json!(self.dir.join(format!("{}/saves/steam_1.json", pc.1.to_hex())));
        args["archive"] = json!(self.dir.join("archive"));
        args["primary"] = json!(primary);
        let out: Value = serde_json::from_str(&call("sync_saves", &args.to_string())).unwrap();
        assert!(out.get("error").is_none() && out.get("unreachable").is_none(), "{} with {}: {out}", self.name, pc.0.name);
        out
    }
}

fn write_at(path: &Path, contents: &str, secs: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    fs::File::options().write(true).open(path).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn three_handhelds_and_three_computers_in_a_partial_mesh() {
    let (h1, h2, h3) = (Handheld::new("H1"), Handheld::new("H2"), Handheld::new("H3"));
    let c1 = start("PC-1", &["steam:1", "steam:2"], vec![h1.id]);
    let c2 = start("PC-2", &["steam:1"], vec![h1.id, h2.id]);
    let c3 = start("PC-3", &["gog:5"], vec![h2.id, h3.id]);

    // A handheld a computer is not paired with is refused.
    let refused: Value = serde_json::from_str(&call(
        "sync_library",
        &{
            let mut a = h3.args(&c1);
            a["state"] = json!(h3.dir.join("refused.json"));
            a["scan"] = json!([]);
            a
        }
        .to_string(),
    ))
    .unwrap();
    assert!(refused.get("error").is_some() || refused.get("unreachable").is_some(), "{refused}");

    // The library relays across the mesh in one round.
    for (h, c) in [(&h1, &c1), (&h1, &c2), (&h2, &c2), (&h2, &c3), (&h3, &c3)] {
        h.library(c);
    }
    let lib = Library::load(&h3.dir.join("library.json")).unwrap();
    let game = &lib.games["steam:1"];
    let on: Vec<&str> = game.installs.values().map(|d| d.device_name.as_str()).collect();
    assert_eq!(game.installs.len(), 2, "one entry, installed on two computers: {on:?}");
    assert!(on.contains(&"PC-1") && on.contains(&"PC-2"));
    assert!(lib.games.contains_key("steam:2") && lib.games.contains_key("gog:5"));
    // And back the other way: PC-3's game reaches H1 through H2 and PC-2.
    h2.library(&c2);
    h1.library(&c2);
    assert!(Library::load(&h1.dir.join("library.json")).unwrap().games.contains_key("gog:5"));

    // Organization travels too, newest change per field (Droidtop/tracker#469):
    // H1 rates a game and puts it in a collection; H3, three hops away,
    // is told to write both.
    let unset = json!({ "steam:1": { "rating": 0.0, "collections": [], "title": "" } });
    h1.library_with(&c2, json!({ "steam:1": { "rating": 0.8, "collections": ["Done", "Platformers"], "title": "" } }));
    // H2 does not have the game, so it reports no marks on it and only relays.
    h2.library(&c2);
    h2.library(&c3);
    let told = h3.library_with(&c3, unset.clone());
    assert_eq!(told["marks"], json!({ "steam:1": { "rating": 0.8, "collections": ["Done", "Platformers"] } }), "{told}");

    // Saves: PC-1's reach H1, then PC-2, then H2.
    write_at(&c1.0.save(), "pc-1 v1", 1_000);
    assert_eq!(h1.saves(&c1, false)["from"], "there");
    assert_eq!(h1.saves(&c2, false)["from"], "here");
    assert_eq!(h2.saves(&c2, false)["from"], "there");
    assert_eq!(read(&h2.save()), "pc-1 v1");

    // H1 and H2 both play: H1's reaches PC-2 first; H2's is newer, so it wins
    // there and PC-2 keeps H1's set as its own copy.
    write_at(&h1.save(), "h1 v2", 2_000);
    write_at(&h2.save(), "h2 v2", 3_000);
    assert_eq!(h1.saves(&c2, false)["from"], "here");
    let newer = h2.saves(&c2, false);
    assert_eq!((newer["from"].as_str(), newer["kept"].as_bool()), (Some("here"), Some(true)), "{newer}");
    assert_eq!(read(&c2.0.save()), "h2 v2");
    assert_eq!(read(&copy_dir(&c2.0.dir.join("archive"), "PC-2").join("winAppData/Game/slot.sav")), "h1 v2");
    // H1 then takes the newest from PC-2.
    assert_eq!(h1.saves(&c2, false)["from"], "there");
    assert_eq!(read(&h1.save()), "h2 v2");

    // H3 gets H2's saves through the computer they share.
    assert_eq!(h2.saves(&c3, false)["from"], "here");
    assert_eq!(h3.saves(&c3, false)["from"], "there");
    assert_eq!(read(&h3.save()), "h2 v2");

    // Both changed, and H1 prefers PC-1: its older copy wins, and H1 keeps its own.
    write_at(&c1.0.save(), "pc-1 v3", 4_000);
    write_at(&h1.save(), "h1 v3", 5_000);
    let preferred = h1.saves(&c1, true);
    assert_eq!((preferred["from"].as_str(), preferred["kept"].as_bool()), (Some("there"), Some(true)), "{preferred}");
    assert_eq!(read(&h1.save()), "pc-1 v3");
    assert_eq!(read(&copy_dir(&h1.dir.join("archive"), "H1").join("winAppData/Game/slot.sav")), "h1 v3");
}

#[test]
fn a_computer_that_moved_to_windowcasts_identity_tells_its_handhelds() {
    let h = Handheld::new("Mover");
    let (old, new) = (DeviceKey::generate(), DeviceKey::generate());
    let (old_id, new_id) = (old.peer_id(), new.peer_id());
    let computer: &'static Computer =
        Box::leak(Box::new(Computer { name: "PC".into(), dir: temp("moved"), library: Mutex::new(Library::default()) }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = listener.local_addr().unwrap();
    let paired = h.id;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (Ok((mut ch, which)), moved) = (Channel::accept_any(stream, &[&new, &old], |p| *p == paired), Moved::new(&old, &new))
            else {
                continue;
            };
            let _ = if which == 0 { serve(&mut ch, computer) } else { serve_moved(&mut ch, computer, Some(moved)) };
        }
    });
    // Reached at the identity it had: it names the new one, signed by both.
    let out = h.library(&(computer, old_id, at));
    assert_eq!(out["moved_to"], json!(new_id.to_hex()), "{out}");
    // Re-pinned, the handheld reaches it at the new one, and hears nothing more.
    let again = h.library(&(computer, new_id, at));
    assert!(again.get("moved_to").is_none(), "{again}");
}
