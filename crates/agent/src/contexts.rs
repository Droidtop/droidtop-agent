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

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use droidtop_agent_core::context::{RecordChange, Records};
use serde_json::{json, Value};

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
    let mut child = Command::new(program)
        .arg(verb)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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

impl Agent {
    /// The adapter the person added for context [`id`].
    pub fn adapter(&self, id: &str) -> Option<Adapter> {
        self.settings.lock().unwrap().adapters.get(id).map(|program| Adapter { id: id.to_string(), program: program.clone() })
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
