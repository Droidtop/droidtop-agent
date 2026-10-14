//! Both ends of a session in one process: a handheld syncing one game's saves,
//! its library and a plugin context with a computer over loopback TCP. The
//! handheld side is the same code droidtop calls through JNI.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::context::{RecordChange, Records};
use droidtop_agent_core::keys::{DeviceKey, PeerId};
use droidtop_agent_core::library::{Change, Library};
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_core::savesync::{self, Outcome, SaveSyncRequest, Side};
use droidtop_agent_core::server::{serve, Host};
use droidtop_agent_core::Result;

struct TestHost {
    roots: Roots,
    archive: PathBuf,
    library: Mutex<Library>,
    records: Mutex<Records>,
    app_running: bool,
}

impl Host for TestHost {
    fn name(&self) -> String {
        "Test PC".into()
    }
    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)> {
        (game.key == "steam:1").then(|| (SaveSpec { patterns: vec!["<winAppData>/Game".into()] }, self.roots.clone()))
    }
    fn archive_dir(&self, _game: &GameRef) -> PathBuf {
        self.archive.clone()
    }
    fn library_pull(&self, _peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)> {
        Ok(self.library.lock().unwrap().changes_since(since))
    }
    fn library_push(&self, _peer: &PeerId, changes: Vec<Change>) -> Result<()> {
        let mut lib = self.library.lock().unwrap();
        for c in changes {
            lib.apply(c);
        }
        Ok(())
    }
    fn context_pull(&self, _context: &str, _offer: Option<&droidtop_agent_core::context::AdapterOffer>) -> Result<Records> {
        Ok(self.records.lock().unwrap().clone())
    }
    fn context_push(&self, _context: &str, changes: Vec<RecordChange>) -> Result<Option<String>> {
        if self.app_running {
            return Ok(Some("F95Checker is open".into()));
        }
        let mut records = self.records.lock().unwrap();
        for c in changes {
            match c {
                RecordChange::Upsert { key, fields } => records.entry(key).or_default().extend(fields),
                RecordChange::Remove { key } => {
                    records.remove(&key);
                }
            }
        }
        Ok(None)
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dta-e2e-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn roots(dir: &Path) -> Roots {
    let mut r = Roots::new();
    r.insert("<winAppData>".into(), dir.join("AppData/Roaming"));
    fs::create_dir_all(dir.join("AppData/Roaming")).unwrap();
    r
}

/// Runs [`client`] against a computer serving [`host`] for one session.
fn session<T>(host: &'static TestHost, client: impl FnOnce(&mut Channel<TcpStream>) -> T) -> T {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let pc = DeviceKey::generate();
    let pc_id = pc.peer_id();
    let handheld = DeviceKey::generate();
    let hh_id = handheld.peer_id();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut ch = Channel::accept(s, &pc, |p| *p == hh_id).unwrap();
        serve(&mut ch, host).unwrap();
    });
    let mut ch = Channel::connect(TcpStream::connect(addr).unwrap(), &handheld, &pc_id).unwrap();
    let out = client(&mut ch);
    ch.send_json(&droidtop_agent_core::proto::Request::Bye).unwrap();
    server.join().unwrap();
    out
}

fn sync(host: &'static TestHost, hh_roots: &Roots, state: &Path, choice: Option<Side>) -> Outcome {
    let baseline = state.join("baseline.json");
    let archive = state.join("archive");
    session(host, |ch| {
        let req = SaveSyncRequest {
            game: GameRef { key: "steam:1".into(), title: "Game".into() },
            roots: hh_roots,
            baseline_path: &baseline,
            archive_dir: &archive,
            name: "Handheld",
            primary_computer: false,
            choice,
        };
        savesync::sync(ch, &req).unwrap()
    })
}

/// Writes a save with a given modification time, so which copy is newest is clear.
fn write_at(path: &Path, contents: &[u8], secs: u64) {
    fs::write(path, contents).unwrap();
    fs::File::options().write(true).open(path).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
}

#[test]
fn saves_sync_both_ways_and_the_newest_copy_wins() {
    let pc_dir = temp("pc");
    let hh_dir = temp("hh");
    let state = temp("state");
    let pc_roots = roots(&pc_dir);
    let hh_roots = roots(&hh_dir);
    let pc_game = pc_dir.join("AppData/Roaming/Game");
    let hh_game = hh_dir.join("AppData/Roaming/Game");
    fs::create_dir_all(pc_game.join("slots")).unwrap();
    fs::write(pc_game.join("slots/one.sav"), b"pc save 1").unwrap();
    fs::write(pc_game.join("settings.ini"), b"volume=3").unwrap();
    let host: &'static TestHost = Box::leak(Box::new(TestHost {
        roots: pc_roots,
        archive: pc_dir.join("archive"),
        library: Mutex::new(Library::default()),
        records: Mutex::new(Records::new()),
        app_running: false,
    }));

    // First sync: the handheld has nothing, so the computer's saves come down.
    let first = sync(host, &hh_roots, &state, None);
    assert!(matches!(first, Outcome::Copied { from: Side::There, files: 2, .. }), "{first:?}");
    assert_eq!(fs::read(hh_game.join("slots/one.sav")).unwrap(), b"pc save 1");

    // Nothing changed: up to date.
    assert!(matches!(sync(host, &hh_roots, &state, None), Outcome::UpToDate { files: 2 }));

    // Played on the handheld: the new save goes up, a deleted file is deleted there.
    fs::write(hh_game.join("slots/one.sav"), b"handheld save 2").unwrap();
    fs::remove_file(hh_game.join("settings.ini")).unwrap();
    let up = sync(host, &hh_roots, &state, None);
    assert!(matches!(up, Outcome::Copied { from: Side::Here, files: 1, removed: 1, .. }), "{up:?}");
    assert_eq!(fs::read(pc_game.join("slots/one.sav")).unwrap(), b"handheld save 2");
    assert!(!pc_game.join("settings.ini").exists());

    // Played on both: the newest copy wins, and the computer keeps its own.
    write_at(&hh_game.join("slots/one.sav"), b"handheld save 3", 2_000_000_000);
    write_at(&pc_game.join("slots/one.sav"), b"pc save 3", 1_900_000_000);
    let newest = sync(host, &hh_roots, &state, None);
    assert!(matches!(newest, Outcome::Copied { from: Side::Here, kept: true, .. }), "{newest:?}");
    assert_eq!(fs::read(pc_game.join("slots/one.sav")).unwrap(), b"handheld save 3");
    let kept = savesync::copy_dir(&pc_dir.join("archive"), "Test PC").join("winAppData/Game/slots/one.sav");
    assert_eq!(fs::read(&kept).unwrap(), b"pc save 3");

    // Played on both again, and the person picks the computer's older copy:
    // the handheld keeps its own as a copy.
    write_at(&hh_game.join("slots/one.sav"), b"handheld save 4", 2_100_000_000);
    write_at(&pc_game.join("slots/one.sav"), b"pc save 4", 2_050_000_000);
    let picked = sync(host, &hh_roots, &state, Some(Side::There));
    assert!(matches!(picked, Outcome::Copied { from: Side::There, kept: true, .. }), "{picked:?}");
    assert_eq!(fs::read(hh_game.join("slots/one.sav")).unwrap(), b"pc save 4");
    let own = savesync::copy_dir(&state.join("archive"), "Handheld").join("winAppData/Game/slots/one.sav");
    assert_eq!(fs::read(own).unwrap(), b"handheld save 4");

    // A game the computer knows no saves for says so.
    let unknown = session(host, |ch| {
        let req = SaveSyncRequest {
            game: GameRef { key: "steam:2".into(), title: "Other".into() },
            roots: &hh_roots,
            baseline_path: &state.join("other.json"),
            archive_dir: &state.join("archive"),
            name: "Handheld",
            primary_computer: false,
            choice: None,
        };
        savesync::sync(ch, &req).unwrap()
    });
    assert_eq!(unknown, Outcome::NoSpec);

    for d in [pc_dir, hh_dir, state] {
        let _ = fs::remove_dir_all(d);
    }
}

#[test]
fn saves_left_in_the_share_state_what_they_were_made_against() {
    use droidtop_agent_core::mailbox::{self, Envelope};
    use droidtop_agent_core::savesync::{post_to_share, Posted};

    let pc_dir = temp("share-pc");
    let hh_dir = temp("share-hh");
    let state = temp("share-state");
    let share = temp("share-folder");
    let pc_roots = roots(&pc_dir);
    let hh_roots = roots(&hh_dir);
    let pc_game = pc_dir.join("AppData/Roaming/Game");
    fs::create_dir_all(&pc_game).unwrap();
    fs::write(pc_game.join("one.sav"), b"pc 1").unwrap();
    let host: &'static TestHost = Box::leak(Box::new(TestHost {
        roots: pc_roots,
        archive: pc_dir.join("archive"),
        library: Mutex::new(Library::default()),
        records: Mutex::new(Records::new()),
        app_running: false,
    }));
    let game = GameRef { key: "steam:1".into(), title: "Game".into() };
    let baseline = state.join("baseline.json");
    let handheld = DeviceKey::generate();
    let pc = DeviceKey::generate();
    let post = || post_to_share(&share, &handheld, &pc.peer_id(), &game, &hh_roots, &baseline, false).unwrap();

    // Before any live sync the handheld does not know where the saves are.
    assert_eq!(post(), Posted::NotYet);
    assert!(matches!(sync(host, &hh_roots, &state, None), Outcome::Copied { from: Side::There, .. }));
    assert_eq!(post(), Posted::UpToDate { files: 1 });

    // Played while the computer is away: the set goes to the share, made
    // against the live baseline.
    let hh_save = hh_dir.join("AppData/Roaming/Game/one.sav");
    fs::write(&hh_save, b"handheld 2").unwrap();
    assert!(matches!(post(), Posted::Posted { files: 1, .. }));
    // Played again before the computer took it: the next set is made
    // against the one already sent.
    fs::write(&hh_save, b"handheld 3").unwrap();
    assert!(matches!(post(), Posted::Posted { files: 1, .. }));
    let (letters, failed) = mailbox::collect(&share, &pc, |p| *p == handheld.peer_id()).unwrap();
    assert!(failed.is_empty());
    let mut sets: Vec<(Vec<String>, Vec<u8>)> = letters
        .iter()
        .map(|l| match mailbox::unpack(&l.payload).unwrap() {
            (Envelope::Saves { base, files, .. }, contents) => {
                assert_eq!(files.len(), 1);
                (base.into_iter().map(|e| e.sha256).collect(), contents.to_vec())
            }
            (other, _) => panic!("{other:?}"),
        })
        .collect();
    sets.sort_by_key(|(_, c)| c.clone());
    assert_eq!(sets[0].1, b"handheld 2");
    assert_eq!(sets[1].1, b"handheld 3");
    // The second was made against the first.
    let first_digest = droidtop_agent_core::hex::encode(&<sha2::Sha256 as sha2::Digest>::digest(b"handheld 2"));
    assert_eq!(sets[1].0, vec![first_digest]);

    // A live sync settles both sides and forgets what was posted.
    assert!(matches!(sync(host, &hh_roots, &state, None), Outcome::Copied { from: Side::Here, .. }));
    assert_eq!(post(), Posted::UpToDate { files: 1 });

    for d in [pc_dir, hh_dir, state, share] {
        let _ = fs::remove_dir_all(d);
    }
}

#[test]
fn library_and_context_travel_over_the_channel() {
    use droidtop_agent_core::proto::{Request, Response};
    use serde_json::json;

    let pc = DeviceKey::generate().peer_id();
    let mut pc_lib = Library::default();
    pc_lib.update_device(
        &pc,
        "Test PC",
        vec![droidtop_agent_core::library::ScannedGame {
            key: "steam:1".into(),
            title: "Game".into(),
            platform: None,
            install: Default::default(),
        }],
    );
    let mut records = Records::new();
    records.insert("100".into(), [("installed".to_string(), json!("0.1"))].into_iter().collect());
    let host: &'static TestHost = Box::leak(Box::new(TestHost {
        roots: Roots::new(),
        archive: std::env::temp_dir(),
        library: Mutex::new(pc_lib),
        records: Mutex::new(records),
        app_running: false,
    }));
    let pulled = session(host, |ch| {
        ch.send_json(&Request::LibraryPull { since: 0 }).unwrap();
        let reply: Response = ch.recv_json().unwrap();
        ch.send_json(&Request::ContextPush {
            context: "f95checker".into(),
            changes: vec![RecordChange::Upsert {
                key: "100".into(),
                fields: [("installed".to_string(), json!("0.2"))].into_iter().collect(),
            }],
        })
        .unwrap();
        let applied: Response = ch.recv_json().unwrap();
        (reply, applied)
    });
    match pulled.0 {
        Response::LibraryChanges { changes, cursor } => {
            assert_eq!(changes.len(), 1);
            assert_eq!(cursor, 1);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(pulled.1, Response::ContextApplied { deferred: false, .. }));
    assert_eq!(host.records.lock().unwrap()["100"]["installed"], json!("0.2"));
}
