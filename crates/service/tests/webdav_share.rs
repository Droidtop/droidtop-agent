//! Store and forward through a WebDAV share (docs/DESIGN.md section 10, transport 3;
//! Droidtop/tracker#800), against a WebDAV server in this process: a handheld leaves a letter, the
//! agent opens it, removes it and answers in the handheld's inbox. The share only ever receives
//! sealed letters, and the share's password is kept sealed on the computer.
#![cfg(feature = "webdav")]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use droidtop_agent_core::keys::DeviceKey;
use droidtop_agent_core::mailbox::{self, Envelope};
use droidtop_agent_core::pairing::Kind;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::share::Share;
use droidtop_agent_service::webdav::{self, RemoteShare};
use droidtop_agent_service::{share, Agent};
use droidtop_agent_store::Dirs;
use net_remote::{WebDav, WebDavConfig};

const USER: &str = "me";
const PASSWORD: &str = "correct horse battery staple";

/// A WebDAV server's state: each path, with `None` for a folder; every request seen; every body put.
#[derive(Default)]
struct Dav {
    items: BTreeMap<String, Option<Vec<u8>>>,
    requests: Vec<String>,
    bodies: Vec<Vec<u8>>,
}

/// A small WebDAV server on loopback, enough of RFC 4918 for a share client: PROPFIND (depth 0 and 1),
/// GET, PUT, MOVE, DELETE and MKCOL, behind Basic sign-in. Its address and its state.
fn dav_server() -> (String, Arc<Mutex<Dav>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/dav", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(Dav::default()));
    state.lock().unwrap().items.insert(String::new(), None);
    let shared = state.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let state = shared.clone();
            thread::spawn(move || connection(stream, &state));
        }
    });
    (url, state)
}

fn connection(stream: TcpStream, state: &Mutex<Dav>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut headers = BTreeMap::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).unwrap();
        let (status, reply) = answer(state, &method, &target, &headers, body);
        let reason = match status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            207 => "Multi-Status",
            401 => "Unauthorized",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            _ => "Other",
        };
        let head = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/xml\r\n\r\n", reply.len());
        if out.write_all(head.as_bytes()).and_then(|_| out.write_all(&reply)).is_err() {
            return;
        }
    }
}

/// A path under `/dav` (the letters' names need no decoding), without slashes at either end; `None` for anything outside it.
fn local(target: &str) -> Option<String> {
    let path =
        target.split_once("://").map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or("")).unwrap_or(target.trim_start_matches('/'));
    let rest = path.strip_prefix("dav")?;
    Some(rest.trim_matches('/').to_string())
}

fn parent(path: &str) -> String {
    path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default()
}

fn answer(state: &Mutex<Dav>, method: &str, target: &str, headers: &BTreeMap<String, String>, body: Vec<u8>) -> (u16, Vec<u8>) {
    let mut dav = state.lock().unwrap();
    dav.requests.push(format!("{method} {target}"));
    let expected = format!("Basic {}", base64_encode(&format!("{USER}:{PASSWORD}")));
    if headers.get("authorization") != Some(&expected) {
        return (401, Vec::new());
    }
    let Some(path) = local(target) else { return (404, Vec::new()) };
    match method {
        "PROPFIND" => {
            let Some(item) = dav.items.get(&path) else { return (404, Vec::new()) };
            let mut listed = vec![(path.clone(), item.clone())];
            if item.is_none() && headers.get("depth").map(String::as_str) == Some("1") {
                for (p, i) in &dav.items {
                    if !p.is_empty() && parent(p) == path && *p != path {
                        listed.push((p.clone(), i.clone()));
                    }
                }
            }
            let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
            for (p, i) in listed {
                let kind = match &i {
                    None => "<d:resourcetype><d:collection/></d:resourcetype>".to_string(),
                    Some(b) => format!("<d:resourcetype/><d:getcontentlength>{}</d:getcontentlength>", b.len()),
                };
                xml.push_str(&format!(
                    "<d:response><d:href>/dav/{p}</d:href><d:propstat><d:prop>{kind}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                ));
            }
            xml.push_str("</d:multistatus>");
            (207, xml.into_bytes())
        }
        "GET" => match dav.items.get(&path) {
            Some(Some(b)) => (200, b.clone()),
            _ => (404, Vec::new()),
        },
        "PUT" => {
            if !matches!(dav.items.get(&parent(&path)), Some(None)) {
                return (409, Vec::new());
            }
            dav.bodies.push(body.clone());
            dav.items.insert(path, Some(body));
            (201, Vec::new())
        }
        "MKCOL" => {
            if dav.items.contains_key(&path) {
                return (405, Vec::new());
            }
            if !matches!(dav.items.get(&parent(&path)), Some(None)) {
                return (409, Vec::new());
            }
            dav.items.insert(path, None);
            (201, Vec::new())
        }
        "MOVE" => {
            let Some(to) = headers.get("destination").and_then(|d| local(d)) else { return (409, Vec::new()) };
            match dav.items.remove(&path) {
                Some(item) => {
                    dav.items.insert(to, item);
                    (201, Vec::new())
                }
                None => (404, Vec::new()),
            }
        }
        "DELETE" => match dav.items.remove(&path) {
            Some(_) => (204, Vec::new()),
            None => (404, Vec::new()),
        },
        _ => (405, Vec::new()),
    }
}

fn base64_encode(text: &str) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn agent(name: &str) -> &'static Agent {
    // Each run gets a folder of its own under the system's temporary folder.
    let root: PathBuf =
        std::env::temp_dir().join(format!("dta-webdav-{}-{name}-{}", std::process::id(), droidtop_agent_core::library::now_ms()));
    let dirs = Dirs { config: root.join("config"), data: root.join("data"), computer: root.join("computer") };
    for d in [&dirs.config, &dirs.data, &dirs.computer] {
        fs::create_dir_all(d).unwrap();
    }
    let agent: &'static Agent = Box::leak(Box::new(Agent::open_in(dirs).unwrap()));
    agent.store.settings.lock().unwrap().rendezvous = false;
    agent.store.save_settings().unwrap();
    agent
}

fn handheld_view(url: &str, password: &str) -> RemoteShare<WebDav> {
    RemoteShare(WebDav::new(WebDavConfig::new(url).sign_in(USER, password).allow_plain_http(true)).unwrap())
}

#[test]
fn letters_go_both_ways_through_a_webdav_share_and_it_sees_only_ciphertext() {
    let (url, dav) = dav_server();
    let computer = agent("both-ways");
    let handheld = DeviceKey::generate();
    computer.store.add_device(handheld.peer_id(), "Handheld", Kind::Handheld).unwrap();
    webdav::set(computer, &url, Some(USER), Some(PASSWORD), true).unwrap();

    // The handheld leaves a save set for a game this computer has no save location for.
    let theirs = handheld_view(&url, PASSWORD);
    let game = GameRef { key: "steam:4242".into(), title: "Secret Plaintext Title".into() };
    let letter = mailbox::pack(&Envelope::Saves { game: game.clone(), base: Vec::new(), files: Vec::new() }, &[]).unwrap();
    let posted = mailbox::post_via(&theirs, &handheld, &computer.store.peer_id(), &letter).unwrap();

    // The agent's minute: it opens the letter from the WebDAV share, removes it, and answers.
    share::process(computer).unwrap();
    assert!(theirs.files(&mailbox::inbox_path(&computer.store.peer_id())).unwrap().is_empty());
    assert!(dav.lock().unwrap().requests.iter().any(|r| r.starts_with("DELETE") && r.ends_with(posted.rsplit('/').next().unwrap())));

    let (letters, failed) = mailbox::collect_via(&theirs, &handheld, |p| *p == computer.store.peer_id()).unwrap();
    assert!(failed.is_empty());
    // The refusal, beside any library letter the agent leaves the handheld at the same time.
    let refusals: Vec<_> = letters
        .iter()
        .filter_map(|l| match mailbox::unpack(&l.payload).unwrap().0 {
            Envelope::SavesRefused { game, reason } => Some((game, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].0, game);
    assert!(refusals[0].1.contains("no save location"), "{}", refusals[0].1);

    // Everything the share received was a sealed letter: neither the title nor the reason is in it.
    let state = dav.lock().unwrap();
    assert_eq!(state.bodies.len(), 1 + letters.len());
    for body in &state.bodies {
        assert_eq!(&body[..4], b"DTM1");
        let text = String::from_utf8_lossy(body);
        assert!(!text.contains("Secret Plaintext Title") && !text.contains("save location"));
    }
    drop(state);

    // The password is on the computer only sealed: not in the settings file, not in what it prints.
    let saved = fs::read_to_string(computer.store.dirs.config.join("webdav.json")).unwrap();
    assert!(!saved.contains(PASSWORD));
    let current = webdav::current(computer).unwrap().unwrap();
    assert!(!format!("{current:?}").contains(PASSWORD));
    assert_eq!(webdav::open_secret(computer, current.password_sealed.as_deref().unwrap()).unwrap(), PASSWORD);
    // Another identity cannot open it.
    let other = agent("other-identity");
    assert!(webdav::open_secret(other, current.password_sealed.as_deref().unwrap()).is_err());
}

#[test]
fn a_refused_sign_in_and_plain_http_are_reported() {
    let (url, _dav) = dav_server();
    let computer = agent("refused");
    webdav::set(computer, &url, Some(USER), Some("wrong"), true).unwrap();
    let err = share::process(computer).unwrap_err();
    assert!(err.to_lowercase().contains("sign"), "{err}");

    // Plain http only when the person allows it; the share is not saved otherwise.
    let fresh = agent("plain-http");
    assert!(webdav::set(fresh, &url, Some(USER), Some(PASSWORD), false).is_err());
    assert!(webdav::current(fresh).unwrap().is_none());

    webdav::clear(computer).unwrap();
    assert!(webdav::current(computer).unwrap().is_none());
    share::process(computer).unwrap();
}
