//! The computer's side of plugin context sync (docs/DESIGN.md section 8):
//! context adapters. An adapter is a separate program that knows one kind of
//! third-party data (the F95 plugin's adapter reads F95Checker's
//! `db.sqlite3`); the agent runs it for each pull and push and speaks JSON
//! with it over its standard input and output. The agent itself knows no
//! third-party format, so an adapter lives with the plugin it serves and
//! ships on its own schedule.
//!
//! The contract, protocol 1:
//! - `<adapter> describe` prints `{"protocol": 1, "id": "<context>",
//!   "description": "..."}`.
//! - `<adapter> pull` prints `{"records": {<key>: {<field>: <value>}}}`.
//! - `<adapter> push` reads `{"changes": [<RecordChange>...]}` and prints
//!   `{"deferred": null}`, or `{"deferred": "<why>"}` when the changes must
//!   wait (the app that owns the data is open).
//! - Any of them may print `{"error": "<words a screen can show>"}`; a
//!   non-zero exit is an error too, with what it wrote to standard error.
//!
//! An adapter reads only the fields its context declares and writes only
//! the ones that may travel to the computer, and it writes only while the
//! app that owns the data is closed.
//!
//! A crate of its own (docs/DESIGN.md section 13), over this computer's
//! store: the running agent runs adapters for handhelds, the window and
//! the command line approve and remove them.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::exec_gate;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use droidtop_agent_store::{read_json, write_json, Approval, Settings, Store};

/// The adapter protocols this agent speaks. Protocol 1 (`pull` and `push`
/// against the app's live data) stays readable until every published adapter
/// has moved to protocol 2, where the adapter publishes a copy and is told
/// when one arrived (`received`; docs/DESIGN.md section 8).
pub const PROTOCOL: u64 = 2;
const OLDEST_PROTOCOL: u64 = 1;

/// How long one adapter run may take before it is stopped.
const LIMIT: Duration = Duration::from_secs(120);

/// One context adapter: the context it serves and its program.
pub struct Adapter {
    pub id: String,
    pub program: PathBuf,
}

/// Runs [`program`] with [`verb`], feeding it [`input`], and reads the one
/// JSON object it prints.
fn run(program: &Path, verb: &str, input: Option<Value>) -> Result<Value, String> {
    let name = program.display();
    // Started through the exec gate, so an adapter written a moment ago is never open for writing in a child another thread
    // is starting (Linux's "text file busy").
    let mut child = exec_gate::spawn(
        Command::new(program)
            .arg(verb)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|e| format!("the context adapter {name} could not start ({e})"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        let mut bytes = serde_json::to_vec(&input).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        // Written on its own thread, so an adapter that prints before it has
        // read everything cannot leave both sides waiting on a full pipe.
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the context adapter {name} took longer than {} s and was stopped", LIMIT.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    let reply: Option<Value> = serde_json::from_slice(&out).ok();
    if let Some(message) = reply.as_ref().and_then(|r| r["error"].as_str()) {
        return Err(message.to_string());
    }
    if !status.success() {
        let said = String::from_utf8_lossy(&err).trim().to_string();
        let said = if said.is_empty() { String::new() } else { format!(": {said}") };
        return Err(format!("the context adapter {name} failed ({status}){said}"));
    }
    reply.ok_or_else(|| format!("the context adapter {name} printed something other than JSON"))
}

/// What [`program`] says about itself: its context and a line about it.
pub fn describe(program: &Path) -> Result<(String, String), String> {
    let reply = run(program, "describe", None)?;
    let protocol = reply["protocol"].as_u64().unwrap_or(0);
    if !(OLDEST_PROTOCOL..=PROTOCOL).contains(&protocol) {
        return Err(format!("{} speaks adapter protocol {protocol}; this agent speaks {OLDEST_PROTOCOL} to {PROTOCOL}", program.display()));
    }
    let id = reply["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64).ok_or("the adapter named no context")?;
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(format!("{id} is not a context name"));
    }
    Ok((id.to_string(), reply["description"].as_str().unwrap_or_default().to_string()))
}

impl Adapter {
    /// The protocol the program speaks now (it is asked each time, so an
    /// adapter replaced by a newer one is spoken to as the newer one).
    pub fn protocol(&self) -> Result<u64, String> {
        Ok(run(&self.program, "describe", None)?["protocol"].as_u64().unwrap_or(1))
    }

    /// Tells a protocol 2 adapter that a new copy of its context is current
    /// (`received`, with the copy's generation); it brings the copy into its
    /// own data when that is safe. Ok(Some(reason)) when it must wait (the
    /// app that owns the data is open). A protocol 1 adapter has nothing to
    /// be told.
    pub fn received(&self, generation: u64) -> Result<Option<String>, String> {
        if self.protocol()? < 2 {
            return Ok(None);
        }
        let reply = run(&self.program, "received", Some(json!({ "generation": generation })))?;
        Ok(reply["deferred"].as_str().map(str::to_string))
    }

    pub fn pull(&self) -> Result<Records, String> {
        let reply = run(&self.program, "pull", None)?;
        serde_json::from_value(reply["records"].clone()).map_err(|e| format!("the {} adapter's records could not be read ({e})", self.id))
    }

    /// Ok(Some(reason)) when the changes must wait.
    pub fn push(&self, changes: Vec<RecordChange>) -> Result<Option<String>, String> {
        if changes.is_empty() {
            return Ok(None);
        }
        let reply = run(&self.program, "push", Some(json!({ "changes": changes })))?;
        Ok(reply["deferred"].as_str().map(str::to_string))
    }
}

pub use droidtop_agent_core::copy::valid_context as valid_id;

/// A copy that arrived and that its adapter has not taken in yet: the
/// generation, kept beside the copy until the adapter says it has.
fn pending_path(store: &Store, id: &str) -> PathBuf {
    store.dirs.adapters().join(id).join("copy").join("pending")
}

/// Tells [`id`]'s adapter that the copy of [`generation`] is current, and
/// remembers it when the adapter must wait. Returns why it waits, if it does.
/// Without an adapter (or a protocol 1 one) there is nobody to tell.
pub fn tell_received(store: &Store, id: &str, generation: u64) -> Result<Option<String>, String> {
    let Some(adapter) = adapter(store, id) else { return Ok(None) };
    let pending = pending_path(store, id);
    match adapter.received(generation)? {
        Some(why) => {
            std::fs::write(&pending, generation.to_string()).map_err(|e| e.to_string())?;
            Ok(Some(why))
        }
        None => {
            let _ = std::fs::remove_file(&pending);
            Ok(None)
        }
    }
}

/// Tells every adapter that was told to wait that its copy is current, again:
/// called when something else happens in that context (a copy is read or
/// replaced, or `droidtop-agent contexts publish` runs), so a wait ends when
/// the app that owned the data has closed, with no timer. Returns how many
/// adapters took their copy in.
pub fn deliver_pending(store: &Store) -> usize {
    let ids: Vec<String> = store.settings.lock().unwrap().adapters.keys().cloned().collect();
    ids.iter()
        .filter(|id| {
            let Some(generation) = std::fs::read_to_string(pending_path(store, id)).ok().and_then(|g| g.trim().parse::<u64>().ok()) else {
                return false;
            };
            matches!(tell_received(store, id, generation), Ok(None))
        })
        .count()
}

/// The plugin's computer side publishes a new copy of [`id`] (`updated`;
/// `droidtop-agent contexts publish`): `{"records": {...}, "files": [...]}`.
/// It becomes current in one step, and the paired handhelds read it at their
/// next sync. Returns its generation.
pub fn publish(store: &Store, id: &str, document: &[u8]) -> Result<u64, String> {
    let document: Value = serde_json::from_slice(document).map_err(|e| format!("the copy could not be read ({e})"))?;
    let records: Records =
        serde_json::from_value(document["records"].clone()).map_err(|e| format!("the copy's records could not be read ({e})"))?;
    let files: Vec<droidtop_agent_core::copy::CopyFile> = match document.get("files") {
        None | Some(Value::Null) => Vec::new(),
        Some(files) => serde_json::from_value(files.clone()).map_err(|e| format!("the copy's files could not be read ({e})"))?,
    };
    let generation = droidtop_agent_core::copy::CopyStore::open(&store.dirs.adapters(), id)
        .and_then(|copies| copies.publish(records, files))
        .map_err(|e| e.to_string())?;
    deliver_pending(store);
    Ok(generation)
}

/// The largest adapter program fetched.
const MAX_PROGRAM: u64 = 64 * 1024 * 1024;

fn download(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url} is not an https address"));
    }
    let response = ureq::get(url).call().map_err(|e| format!("the adapter could not be fetched from {url}: {e}"))?;
    let mut bytes = Vec::new();
    response.into_reader().take(MAX_PROGRAM + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROGRAM {
        return Err(format!("the adapter at {url} is larger than {} MiB", MAX_PROGRAM / (1024 * 1024)));
    }
    Ok(bytes)
}

/// The adapter for context [`id`]. One added or approved by another run
/// of the agent (`contexts add`, `contexts approve`) since this one
/// started is read from the settings file.
pub fn adapter(store: &Store, id: &str) -> Option<Adapter> {
    let mut settings = store.settings.lock().unwrap();
    if !settings.adapters.contains_key(id) {
        if let Ok(on_disk) = read_json::<Settings>(&store.dirs.settings()) {
            if let Some(program) = on_disk.adapters.get(id) {
                settings.adapters.insert(id.to_string(), program.clone());
                if let Some(approval) = on_disk.approved.get(id) {
                    settings.approved.insert(id.to_string(), approval.clone());
                }
            }
        }
    }
    settings.adapters.get(id).map(|program| Adapter { id: id.to_string(), program: program.clone() })
}

/// The adapters plugins offered that wait for the person.
pub fn adapter_offers(store: &Store) -> BTreeMap<String, AdapterOffer> {
    read_json(&store.dirs.adapter_offers()).unwrap_or_default()
}

/// What a plugin on the handheld offers for [`context`]:
/// - nothing changes when the person set this context up by hand, or
///   approved another plugin for it;
/// - from the plugin the person approved, a different program is a new
///   version, fetched and checked by its digest at once;
/// - otherwise the offer waits for `contexts approve`, said on the console.
pub fn consider_offer(store: &Store, context: &str, offer: &AdapterOffer) -> Result<(), String> {
    let Some(program) = offer.for_this_system() else { return Ok(()) };
    if !valid_id(context) {
        return Err(format!("{context} is not a context name"));
    }
    let installed = adapter(store, context).is_some();
    let approval = store.settings.lock().unwrap().approved.get(context).cloned();
    match approval {
        Some(a) if a.plugin == offer.plugin => {
            if !a.sha256.eq_ignore_ascii_case(&program.sha256) {
                install(store, context, offer)?;
                println!("Updated the {context} adapter from {}.", offer.plugin);
            }
            Ok(())
        }
        Some(_) => Ok(()),
        None if installed => Ok(()),
        None if store.settings.lock().unwrap().declined.get(context) == Some(&offer.plugin) => Ok(()),
        None => {
            let mut offers = adapter_offers(store);
            if offers.get(context) != Some(offer) {
                offers.insert(context.to_string(), offer.clone());
                write_json(&store.dirs.adapter_offers(), &offers).map_err(|e| e.to_string())?;
                println!(
                    "The plugin {} on a paired handheld offers an adapter for the {context} context. To install it here: droidtop-agent contexts approve {context}",
                    offer.plugin
                );
            }
            Ok(())
        }
    }
}

/// Fetches the program [`offer`] names for this system, checks its
/// digest and that it serves [`context`], and puts it in place.
pub fn install(store: &Store, context: &str, offer: &AdapterOffer) -> Result<PathBuf, String> {
    if !valid_id(context) {
        return Err(format!("{context} is not a context name"));
    }
    let program = offer.for_this_system().ok_or_else(|| format!("{} offers no program for {}", offer.plugin, AdapterOffer::system()))?;
    let bytes = download(&program.url)?;
    let digest = droidtop_agent_core::hex::encode(&Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(&program.sha256) {
        return Err(format!(
            "the program at {} is not the one the plugin names (its SHA-256 is {digest}); nothing was installed",
            program.url
        ));
    }
    let dir = store.dirs.adapters().join(context);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(if cfg!(windows) { "adapter.exe" } else { "adapter" });
    let tmp = dir.join(if cfg!(windows) { "adapter.new.exe" } else { "adapter.new" });
    exec_gate::write_program(&tmp, &bytes).map_err(|e| e.to_string())?;
    let served = describe(&tmp);
    if !matches!(&served, Ok((id, _)) if id == context) {
        let _ = std::fs::remove_file(&tmp);
        return Err(match served {
            Ok((id, _)) => format!("the program serves the {id} context, not {context}; nothing was installed"),
            Err(e) => e,
        });
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    {
        let mut settings = store.settings.lock().unwrap();
        settings.adapters.insert(context.to_string(), path.clone());
        let approval = Approval { plugin: offer.plugin.clone(), sha256: program.sha256.to_lowercase() };
        settings.approved.insert(context.to_string(), approval);
    }
    store.save_settings().map_err(|e| e.to_string())?;
    Ok(path)
}

/// `contexts approve <context>`: the person's one approval for the
/// adapter a plugin offered; later versions from that plugin follow.
pub fn approve(store: &Store, context: &str) -> Result<String, String> {
    let mut offers = adapter_offers(store);
    let offer = offers.get(context).cloned().ok_or_else(|| {
        format!("No plugin has offered an adapter for {context}. Sync the context from the plugin on the handheld first.")
    })?;
    let program = offer.for_this_system().ok_or_else(|| format!("{} offers no program for {}", offer.plugin, AdapterOffer::system()))?;
    println!("{} ({}) from {}", if offer.label.is_empty() { context } else { &offer.label }, offer.plugin, program.url);
    println!("SHA-256 {}", program.sha256);
    install(store, context, &offer)?;
    offers.remove(context);
    write_json(&store.dirs.adapter_offers(), &offers).map_err(|e| e.to_string())?;
    Ok(format!("Installed the {context} adapter from {}. The handheld's next sync of that context uses it.", offer.plugin))
}

/// The person said no to the adapter a plugin offered for [`context`]:
/// the offer goes, and that plugin's offers for it are not kept again.
/// `contexts add`, or approving another plugin's offer, still works.
pub fn decline(store: &Store, context: &str) -> Result<(), String> {
    let mut offers = adapter_offers(store);
    let Some(offer) = offers.remove(context) else { return Ok(()) };
    write_json(&store.dirs.adapter_offers(), &offers).map_err(|e| e.to_string())?;
    store.settings.lock().unwrap().declined.insert(context.to_string(), offer.plugin);
    store.save_settings().map_err(|e| e.to_string())
}

// The stand-in adapters are shell scripts; the contract is the same on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A shell script standing in for an adapter.
    fn stand_in(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dta-adapter-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adapter");
        exec_gate::write_program(&path, format!("#!/bin/sh\n{body}\n").as_bytes()).unwrap();
        path
    }

    #[test]
    fn an_adapter_is_described_pulled_and_pushed_through_json() {
        let program = stand_in(
            "demo",
            r#"case "$1" in
  describe) echo '{"protocol":1,"id":"demo","description":"a stand-in"}' ;;
  pull) echo '{"records":{"100":{"installed":"0.1"}}}' ;;
  push) read line; case "$line" in *'"op":"upsert"'*) echo '{"deferred":"Demo is open"}' ;; *) echo '{"error":"no changes came"}' ;; esac ;;
esac"#,
        );
        assert_eq!(describe(&program).unwrap(), ("demo".to_string(), "a stand-in".to_string()));
        let adapter = Adapter { id: "demo".into(), program: program.clone() };
        assert_eq!(adapter.pull().unwrap()["100"]["installed"], json!("0.1"));
        let change = RecordChange::Upsert { key: "100".into(), fields: [("installed".to_string(), json!("0.2"))].into_iter().collect() };
        assert_eq!(adapter.push(vec![change]).unwrap(), Some("Demo is open".to_string()));
        let _ = std::fs::remove_dir_all(program.parent().unwrap());
    }

    #[test]
    fn a_failing_adapter_says_why() {
        let failing = stand_in("failing", "echo 'the database is locked' >&2; exit 3");
        let e = describe(&failing).unwrap_err();
        assert!(e.contains("the database is locked"), "{e}");
        let newer = stand_in("newer", r#"echo '{"protocol":9,"id":"x"}'"#);
        assert!(describe(&newer).unwrap_err().contains("protocol 9"));
        for p in [failing, newer] {
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }

    fn store(name: &str) -> Store {
        let root = std::env::temp_dir().join(format!("dta-ctx-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = droidtop_agent_store::Dirs { config: root.join("config"), data: root.join("data"), computer: root.join("host") };
        for d in [&dirs.config, &dirs.data, &dirs.computer] {
            std::fs::create_dir_all(d).unwrap();
        }
        Store::open_in(dirs).unwrap()
    }

    /// A protocol 2 adapter that takes a copy in unless the file `open` is beside it.
    fn demo_two(name: &str) -> PathBuf {
        stand_in(
            name,
            r#"case "$1" in
  describe) echo '{"protocol":2,"id":"demo","description":"a copy adapter"}' ;;
  received) read line; if [ -e "$(dirname "$0")/open" ]; then echo '{"deferred":"Demo is open"}'; else echo "$line" > "$(dirname "$0")/took"; echo '{}'; fi ;;
esac"#,
        )
    }

    #[test]
    fn a_protocol_2_adapter_is_told_a_copy_arrived_and_may_ask_to_wait() {
        let program = demo_two("two-told");
        let adapter = Adapter { id: "demo".into(), program: program.clone() };
        assert_eq!(adapter.protocol().unwrap(), 2);
        let dir = program.parent().unwrap().to_path_buf();
        assert_eq!(adapter.received(3).unwrap(), None);
        assert_eq!(std::fs::read_to_string(dir.join("took")).unwrap().trim(), r#"{"generation":3}"#);
        std::fs::write(dir.join("open"), b"").unwrap();
        assert_eq!(adapter.received(4).unwrap(), Some("Demo is open".to_string()));
        // A protocol 1 adapter has nothing to be told and is not run for it.
        let old = stand_in("one", r#"case "$1" in describe) echo '{"protocol":1,"id":"old"}' ;; *) echo '{"error":"not asked"}' ;; esac"#);
        assert_eq!(Adapter { id: "old".into(), program: old.clone() }.received(1).unwrap(), None);
        for p in [program, old] {
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }

    #[test]
    fn a_wait_ends_when_something_next_happens_and_the_app_is_closed() {
        let store = store("wait");
        let program = demo_two("two-wait");
        let dir = program.parent().unwrap().to_path_buf();
        store.settings.lock().unwrap().adapters.insert("demo".into(), program.clone());
        std::fs::create_dir_all(store.dirs.adapters().join("demo/copy")).unwrap();
        std::fs::write(dir.join("open"), b"").unwrap();
        assert_eq!(tell_received(&store, "demo", 5).unwrap(), Some("Demo is open".to_string()));
        assert_eq!(deliver_pending(&store), 0);
        assert!(pending_path(&store, "demo").exists());
        // The app closes; the next thing in that context delivers it.
        std::fs::remove_file(dir.join("open")).unwrap();
        assert_eq!(deliver_pending(&store), 1);
        assert!(!pending_path(&store, "demo").exists());
        assert_eq!(std::fs::read_to_string(dir.join("took")).unwrap().trim(), r#"{"generation":5}"#);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_published_copy_becomes_current_and_a_damaged_one_changes_nothing() {
        let store = store("publish");
        let first = publish(&store, "demo", br#"{"records":{"100":{"installed":"0.1"}}}"#).unwrap();
        let second = publish(&store, "demo", br#"{"records":{"100":{"installed":"0.2"}}}"#).unwrap();
        assert_eq!((first, second), (1, 2));
        assert!(publish(&store, "demo", b"{\"records\": {").is_err());
        assert!(publish(&store, "../x", br#"{"records":{}}"#).is_err());
        let now = droidtop_agent_core::copy::CopyStore::open(&store.dirs.adapters(), "demo").unwrap().current().unwrap();
        assert_eq!((now.generation, now.records["100"]["installed"].clone()), (2, json!("0.2")));
    }
}
