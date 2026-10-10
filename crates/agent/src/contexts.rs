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

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::Agent;

/// The adapter protocol this agent speaks.
pub const PROTOCOL: u64 = 1;

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
    let start = || {
        Command::new(program)
            .arg(verb)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    // A program written a moment ago (an adapter just installed) can still be
    // open for writing in a process another thread is starting, and Linux then
    // refuses to run it ("text file busy") until that process has started.
    let mut started = start();
    for _ in 0..20 {
        match &started {
            Err(e) if e.raw_os_error() == Some(26) && cfg!(unix) => {
                std::thread::sleep(Duration::from_millis(50));
                started = start();
            }
            _ => break,
        }
    }
    let mut child = started.map_err(|e| format!("the context adapter {name} could not start ({e})"))?;
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
    if protocol != PROTOCOL {
        return Err(format!("{} speaks adapter protocol {protocol}; this agent speaks {PROTOCOL}", program.display()));
    }
    let id = reply["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64).ok_or("the adapter named no context")?;
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(format!("{id} is not a context name"));
    }
    Ok((id.to_string(), reply["description"].as_str().unwrap_or_default().to_string()))
}

impl Adapter {
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

/// Whether [`id`] can name a context (and its folder under `adapters/`).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
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

impl Agent {
    /// The adapter for context [`id`]. One added or approved by another run
    /// of the agent (`contexts add`, `contexts approve`) since this one
    /// started is read from the settings file.
    pub fn adapter(&self, id: &str) -> Option<Adapter> {
        let mut settings = self.settings.lock().unwrap();
        if !settings.adapters.contains_key(id) {
            if let Ok(on_disk) = crate::state::read_json::<crate::state::Settings>(&self.dirs.settings()) {
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
    pub fn adapter_offers(&self) -> BTreeMap<String, AdapterOffer> {
        crate::state::read_json(&self.dirs.adapter_offers()).unwrap_or_default()
    }

    /// What a plugin on the handheld offers for [`context`]:
    /// - nothing changes when the person set this context up by hand, or
    ///   approved another plugin for it;
    /// - from the plugin the person approved, a different program is a new
    ///   version, fetched and checked by its digest at once;
    /// - otherwise the offer waits for `contexts approve`, said on the console.
    pub fn consider_offer(&self, context: &str, offer: &AdapterOffer) -> Result<(), String> {
        let Some(program) = offer.for_this_system() else { return Ok(()) };
        if !valid_id(context) {
            return Err(format!("{context} is not a context name"));
        }
        let installed = self.adapter(context).is_some();
        let approval = self.settings.lock().unwrap().approved.get(context).cloned();
        match approval {
            Some(a) if a.plugin == offer.plugin => {
                if !a.sha256.eq_ignore_ascii_case(&program.sha256) {
                    self.install(context, offer)?;
                    println!("Updated the {context} adapter from {}.", offer.plugin);
                }
                Ok(())
            }
            Some(_) => Ok(()),
            None if installed => Ok(()),
            None if self.settings.lock().unwrap().declined.get(context) == Some(&offer.plugin) => Ok(()),
            None => {
                let mut offers = self.adapter_offers();
                if offers.get(context) != Some(offer) {
                    offers.insert(context.to_string(), offer.clone());
                    crate::state::write_json(&self.dirs.adapter_offers(), &offers).map_err(|e| e.to_string())?;
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
    pub fn install(&self, context: &str, offer: &AdapterOffer) -> Result<PathBuf, String> {
        if !valid_id(context) {
            return Err(format!("{context} is not a context name"));
        }
        let program =
            offer.for_this_system().ok_or_else(|| format!("{} offers no program for {}", offer.plugin, AdapterOffer::system()))?;
        let bytes = download(&program.url)?;
        let digest = droidtop_agent_core::hex::encode(&Sha256::digest(&bytes));
        if !digest.eq_ignore_ascii_case(&program.sha256) {
            return Err(format!(
                "the program at {} is not the one the plugin names (its SHA-256 is {digest}); nothing was installed",
                program.url
            ));
        }
        let dir = self.dirs.adapters().join(context);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(if cfg!(windows) { "adapter.exe" } else { "adapter" });
        let tmp = dir.join(if cfg!(windows) { "adapter.new.exe" } else { "adapter.new" });
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
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
            let mut settings = self.settings.lock().unwrap();
            settings.adapters.insert(context.to_string(), path.clone());
            let approval = crate::state::Approval { plugin: offer.plugin.clone(), sha256: program.sha256.to_lowercase() };
            settings.approved.insert(context.to_string(), approval);
        }
        self.save_settings().map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// `contexts approve <context>`: the person's one approval for the
    /// adapter a plugin offered; later versions from that plugin follow.
    pub fn approve(&self, context: &str) -> Result<String, String> {
        let mut offers = self.adapter_offers();
        let offer = offers.get(context).cloned().ok_or_else(|| {
            format!("No plugin has offered an adapter for {context}. Sync the context from the plugin on the handheld first.")
        })?;
        let program =
            offer.for_this_system().ok_or_else(|| format!("{} offers no program for {}", offer.plugin, AdapterOffer::system()))?;
        println!("{} ({}) from {}", if offer.label.is_empty() { context } else { &offer.label }, offer.plugin, program.url);
        println!("SHA-256 {}", program.sha256);
        self.install(context, &offer)?;
        offers.remove(context);
        crate::state::write_json(&self.dirs.adapter_offers(), &offers).map_err(|e| e.to_string())?;
        Ok(format!("Installed the {context} adapter from {}. The handheld's next sync of that context uses it.", offer.plugin))
    }

    /// The person said no to the adapter a plugin offered for [`context`]:
    /// the offer goes, and that plugin's offers for it are not kept again.
    /// `contexts add`, or approving another plugin's offer, still works.
    pub fn decline(&self, context: &str) -> Result<(), String> {
        let mut offers = self.adapter_offers();
        let Some(offer) = offers.remove(context) else { return Ok(()) };
        crate::state::write_json(&self.dirs.adapter_offers(), &offers).map_err(|e| e.to_string())?;
        self.settings.lock().unwrap().declined.insert(context.to_string(), offer.plugin);
        self.save_settings().map_err(|e| e.to_string())
    }
}

// The stand-in adapters are shell scripts; the contract is the same on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A shell script standing in for an adapter.
    fn stand_in(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dta-adapter-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adapter");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
}
