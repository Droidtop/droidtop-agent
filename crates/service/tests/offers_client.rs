//! The side that receives an offer (docs/DESIGN.md section 20), between two
//! real agents paired as computers over loopback: what the other one offers
//! is all this one sees, a game it accepts is copied into its own game
//! folder and recorded, and asking it to play one gets its own answer.

use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;

use droidtop_agent_core::channel::Channel;
use droidtop_agent_core::keys::DeviceKey;
use droidtop_agent_core::library::{Install, ScannedGame};
use droidtop_agent_core::pairing::Kind;
use droidtop_agent_core::proto::{GameRef, Launch};
use droidtop_agent_core::server::serve;
use droidtop_agent_service::Agent;
use droidtop_agent_store::Dirs;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dta-offers-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn agent(name: &str, games: &std::path::Path) -> &'static Agent {
    let root = temp(name);
    let dirs = Dirs { config: root.join("config"), data: root.join("data"), computer: root.join("computer") };
    for d in [&dirs.config, &dirs.data, &dirs.computer] {
        fs::create_dir_all(d).unwrap();
    }
    let agent: &'static Agent = Box::leak(Box::new(Agent::open_in(dirs).unwrap()));
    {
        let mut s = agent.store.settings.lock().unwrap();
        s.name = Some(name.into());
        s.game_folders = vec![games.to_path_buf()];
        s.rendezvous = false;
    }
    agent.store.save_settings().unwrap();
    agent
}

/// Serves [`host`] on loopback, answering only the devices it trusts.
fn serve_on_loopback(host: &'static Agent) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = listener.local_addr().unwrap().to_string();
    let seed = host.store.key.seed();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let key = DeviceKey::from_seed(&seed).unwrap();
            thread::spawn(move || {
                if let Ok(mut ch) = Channel::accept(stream, &key, |p| host.store.is_trusted(p)) {
                    let _ = serve(&mut ch, host);
                }
            });
        }
    });
    at
}

#[test]
fn a_computer_browses_accepts_and_asks_to_play_only_what_it_is_offered() {
    let desk_games = temp("desk-games");
    fs::create_dir_all(desk_games.join("Moonlit Harbor v1.0/data")).unwrap();
    fs::write(desk_games.join("Moonlit Harbor v1.0/MoonlitHarbor.exe"), b"MZ moonlit").unwrap();
    fs::write(desk_games.join("Moonlit Harbor v1.0/data/script.bin"), vec![7u8; 300_000]).unwrap();
    let lap_games = temp("lap-games");
    let (desk, lap) = (agent("Desk", &desk_games), agent("Lap", &lap_games));
    desk.store.add_device(lap.store.peer_id(), "Lap", Kind::Computer).unwrap();
    lap.store.add_device(desk.store.peer_id(), "Desk", Kind::Computer).unwrap();
    desk.rescan();
    // A handheld's game Desk knows of, which Desk never offers another computer.
    let handheld = DeviceKey::generate().peer_id();
    desk.store.edit_library(|lib| {
        let only_there = ScannedGame {
            key: "steam:440".into(),
            title: "Team Fortress 2".into(),
            platform: Some("pc".into()),
            install: Install { installed: true, ..Install::default() },
        };
        (lib.update_device(&handheld, "Pocket", vec![only_there]), true)
    });
    let at = serve_on_loopback(desk);
    lap.store.seen(&desk.store.peer_id(), Some(at));
    let desk_id = desk.store.peer_id();

    // Nothing is offered to a computer until the person offers it.
    let seen = lap.browse(&desk_id).unwrap();
    assert_eq!(seen.name, "Desk");
    assert!(!seen.offered.library && !seen.offered.saves && seen.offered.launch);
    assert!(seen.games.is_empty());
    let game = GameRef { key: "title:moonlit harbor".into(), title: "Moonlit Harbor".into() };
    assert!(lap.accept(&desk_id, &game, &mut |_, _| {}).unwrap_err().contains("does not offer its library"));

    // Desk offers Lap its library: Lap sees Desk's own game, not the handheld's.
    desk.store.set_offer(&lap.store.peer_id().to_hex(), Some(true), None);
    desk.store.save_settings().unwrap();
    let seen = lap.browse(&desk_id).unwrap();
    assert!(seen.offered.library && !seen.offered.saves);
    let titles: Vec<&str> = seen.games.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, ["Moonlit Harbor"], "{:?}", seen.games);
    assert_eq!(seen.games[0].version.as_deref(), Some("1.0"));
    let game = GameRef { key: seen.games[0].key.clone(), title: seen.games[0].title.clone() };

    // Accepting copies it into Lap's game folder under Desk's own folder name, and records it.
    let mut last = (0, 0);
    let accepted = lap.accept(&desk_id, &game, &mut |done, total| last = (done, total)).unwrap();
    assert_eq!(accepted.folder, lap_games.join("Moonlit Harbor v1.0"));
    assert_eq!(fs::read(lap_games.join("Moonlit Harbor v1.0/MoonlitHarbor.exe")).unwrap(), b"MZ moonlit");
    assert_eq!(fs::read(lap_games.join("Moonlit Harbor v1.0/data/script.bin")).unwrap().len(), 300_000);
    assert_eq!(last.0, last.1);
    assert!(!accepted.keep_in_step, "keeping in step is off until the person turns it on");
    assert_eq!(lap.accepted(), std::slice::from_ref(&accepted));
    assert!(lap.browse(&desk_id).unwrap().games[0].accepted);
    // Accepted again, it lands beside the first copy, never on it.
    let again = lap.accept(&desk_id, &game, &mut |_, _| {}).unwrap();
    assert_eq!(again.folder, lap_games.join("Moonlit Harbor v1.0 (2)"));
    lap.set_keep_in_step(&accepted.folder, true).unwrap();
    assert!(lap.accepted().iter().any(|a| a.folder == accepted.folder && a.keep_in_step));

    // Playing it there is Desk's own switch, off until allowed.
    assert_eq!(lap.play_on(&desk_id, &game).unwrap(), Launch::NotAllowed);

    // A device that is not paired is not reached.
    let stranger = DeviceKey::generate().peer_id();
    assert!(lap.browse(&stranger).unwrap_err().contains("not paired"));
}
