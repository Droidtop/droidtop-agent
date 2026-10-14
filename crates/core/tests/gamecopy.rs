//! Copying a game's folder between a handheld and a computer over loopback
//! (Droidtop/tracker#469 part 3): a new version folder beside the old one,
//! resumed after an interruption, checked file by file, and the other way
//! round into the computer's game folder. Test games are made here; no real
//! game is read.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::gamecopy::{self, PART};
use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::Change;
use droidtop_agent_core::proto::{GameRef, Request};
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_core::server::{serve, Host};
use droidtop_agent_core::Result;

struct Pc {
    game: PathBuf,
    inbox: PathBuf,
}

impl Host for Pc {
    fn name(&self) -> String {
        "PC".into()
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
        Ok(Records::new())
    }
    fn context_push(&self, _context: &str, _changes: Vec<RecordChange>) -> Result<Option<String>> {
        Ok(None)
    }
    fn game_folder(&self, game: &GameRef) -> Option<(PathBuf, Option<String>)> {
        (game.key == "title:testgame").then(|| (self.game.clone(), Some("1.1".into())))
    }
    fn game_inbox(&self) -> std::result::Result<PathBuf, String> {
        Ok(self.inbox.clone())
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dta-copy-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A made-up game: a program, a big data file that takes several pieces, and a nested save folder.
fn make_game(dir: &Path, tag: u8) {
    fs::create_dir_all(dir.join("game/saves")).unwrap();
    fs::write(dir.join("Testgame.exe"), [b'M', b'Z', tag]).unwrap();
    let big: Vec<u8> = (0..2_500_000u32).map(|i| (i % 251) as u8 ^ tag).collect();
    fs::write(dir.join("game/data.pak"), big).unwrap();
    fs::write(dir.join("game/saves/readme.txt"), b"made by the test").unwrap();
    fs::write(dir.join("empty.dat"), b"").unwrap();
}

fn session<T>(host: &'static Pc, client: impl FnOnce(&mut Channel<TcpStream>) -> T) -> T {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let pc = DeviceKey::generate();
    let pc_id = pc.peer_id();
    let handheld = DeviceKey::generate();
    let hh = handheld.peer_id();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut ch = Channel::accept(s, &pc, |p| *p == hh).unwrap();
        let _ = serve(&mut ch, host);
    });
    let mut ch = Channel::connect(TcpStream::connect(addr).unwrap(), &handheld, &pc_id).unwrap();
    let out = client(&mut ch);
    let _ = ch.send_json(&Request::Bye);
    server.join().unwrap();
    out
}

fn same_tree(a: &Path, b: &Path) {
    let (fa, fb) = (gamecopy::describe(a, None).unwrap(), gamecopy::describe(b, None).unwrap());
    assert_eq!(fa.files, fb.files, "same names, sizes and times");
    for f in &fa.files {
        assert_eq!(fs::read(a.join(&f.path)).unwrap(), fs::read(b.join(&f.path)).unwrap(), "{}", f.path);
    }
}

#[test]
fn a_newer_version_is_copied_beside_the_old_one_and_resumes() {
    let pc_game = temp("pc-game").join("Testgame v1.1");
    make_game(&pc_game, 7);
    let shelf = temp("hh-games");
    make_game(&shelf.join("Testgame v1.0"), 3);
    let host: &'static Pc = Box::leak(Box::new(Pc { game: pc_game.clone(), inbox: temp("pc-inbox") }));
    let game = GameRef { key: "title:testgame".into(), title: "Testgame".into() };

    // An earlier copy stopped halfway through the big file.
    let part = shelf.join(format!("Testgame v1.1{PART}"));
    fs::create_dir_all(part.join("game")).unwrap();
    let half = fs::read(pc_game.join("game/data.pak")).unwrap()[..1_000_000].to_vec();
    fs::write(part.join("game/data.pak"), half).unwrap();

    let mut seen = Vec::new();
    let copied = session(host, |ch| gamecopy::pull(ch, &game, &shelf, "Testgame v1.1", &mut |d: u64, t: u64| seen.push((d, t))).unwrap());
    assert_eq!(copied.folder, shelf.join("Testgame v1.1"));
    assert_eq!(copied.files, 4);
    let whole: u64 = 3 + 2_500_000 + 16;
    assert_eq!(copied.bytes, whole - 1_000_000, "only what was missing travelled");
    assert_eq!(seen.last(), Some(&(whole, whole)));
    same_tree(&pc_game, &shelf.join("Testgame v1.1"));
    // The old version is untouched, and no part folder is left.
    assert_eq!(fs::read(shelf.join("Testgame v1.0/Testgame.exe")).unwrap(), [b'M', b'Z', 3]);
    assert!(!part.exists());
    // A folder of that name is never overwritten.
    assert!(session(host, |ch| gamecopy::pull(ch, &game, &shelf, "Testgame v1.1", &mut |_: u64, _: u64| {})).is_err());
}

#[test]
fn a_version_goes_to_the_computers_game_folder() {
    let mine = temp("hh-source").join("Testgame v1.2");
    make_game(&mine, 9);
    let inbox = temp("pc-games");
    let host: &'static Pc = Box::leak(Box::new(Pc { game: temp("unused"), inbox: inbox.clone() }));
    let game = GameRef { key: "title:testgame".into(), title: "Testgame".into() };
    let sent = session(host, |ch| gamecopy::push(ch, &game, &mine, "Testgame v1.2", Some("1.2".into()), &mut |_: u64, _: u64| {}).unwrap());
    assert_eq!(sent.folder, inbox.join("Testgame v1.2"));
    same_tree(&mine, &inbox.join("Testgame v1.2"));
}

#[test]
fn a_computer_cannot_name_files_outside_the_folder() {
    let pc_game = temp("evil");
    fs::write(pc_game.join("ok.txt"), b"ok").unwrap();
    let host: &'static Pc = Box::leak(Box::new(Pc { game: pc_game, inbox: temp("evil-inbox") }));
    let game = GameRef { key: "title:testgame".into(), title: "Testgame".into() };
    // Asking for a path that leaves the folder is refused by the computer.
    let refused = session(host, |ch| {
        ch.send_json(&Request::GameFileGet { game: game.clone(), path: "../evil-inbox/x".into(), offset: 0 }).unwrap();
        ch.recv_json::<droidtop_agent_core::proto::Response>().unwrap()
    });
    assert!(matches!(refused, droidtop_agent_core::proto::Response::Error { .. }), "{refused:?}");
}
