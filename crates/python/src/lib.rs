//! The computer's droidtop agent as a Python extension module,
//! `droidtop_agent`. A desktop manager that loads it (Pythia) is this
//! computer's agent while it runs: the same identity, pairings, library,
//! saves, scanner and service as `droidtop-agent run`, in its own process
//! and over the same files, so only one of the two runs on a computer and
//! either can take over from the other (docs/DESIGN.md section 16).
//!
//! One call, JSON in and JSON out, like droidtop's JNI library
//! (`crates/android`): `droidtop_agent.call(op, args_json)`. A failure comes
//! back as `{"error": "..."}` in words a screen can show. Calls that wait
//! (pairing, scanning) release the interpreter while they run.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::library::ScannedGame;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves;
use droidtop_agent_scan::Found;
use droidtop_agent_service::pair;
use droidtop_agent_service::state::{Agent, Settings};
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

static AGENT: OnceLock<Arc<Agent>> = OnceLock::new();
static OPENING: Mutex<()> = Mutex::new(());

/// The agent over this computer's files, opened on first use.
fn agent() -> Result<Arc<Agent>, String> {
    if let Some(a) = AGENT.get() {
        return Ok(a.clone());
    }
    let _one = OPENING.lock().unwrap();
    if let Some(a) = AGENT.get() {
        return Ok(a.clone());
    }
    let opened = Arc::new(Agent::open().map_err(|e| format!("the agent could not open its files: {e}"))?);
    Ok(AGENT.get_or_init(|| opened).clone())
}

/// The service, once started: Ok while it serves, or why it stopped.
static SERVING: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// This computer showing a pairing code: the outcome once there is one.
struct Showing {
    cancel: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<Result<String, String>>>>,
}

static SHOWING: Mutex<Option<Showing>> = Mutex::new(None);

#[pyfunction]
fn call(py: Python<'_>, op: &str, args: &str) -> String {
    let (op, args) = (op.to_string(), args.to_string());
    py.allow_threads(move || {
        let args: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
        match run(&op, &args) {
            Ok(v) => v.to_string(),
            Err(e) => json!({ "error": e }).to_string(),
        }
    })
}

#[pymodule]
fn droidtop_agent(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(call, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("PROTOCOL_VERSION", droidtop_agent_core::PROTOCOL_VERSION)?;
    Ok(())
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing {key}"))
}

fn run(op: &str, args: &Value) -> Result<Value, String> {
    match op {
        "identity" => {
            let a = agent()?;
            Ok(json!({ "name": a.name(), "id": a.peer_id().to_hex(), "config": a.dirs.config, "data": a.dirs.data }))
        }
        "settings" => serde_json::to_value(&*agent()?.settings.lock().unwrap()).map_err(|e| e.to_string()),
        "settings_set" => settings_set(args),
        "devices" => {
            let a = agent()?;
            let primary = a.settings.lock().unwrap().primary_device.clone();
            let devices: Vec<Value> = a
                .devices_now()
                .into_iter()
                .map(|d| {
                    let trusted = PeerId::from_hex(&d.id).is_ok_and(|p| a.is_trusted(&p));
                    let is_primary = primary.as_deref() == Some(d.id.as_str());
                    let mut v = serde_json::to_value(&d).unwrap_or(Value::Null);
                    v["trusted"] = json!(trusted);
                    v["primary"] = json!(is_primary);
                    v
                })
                .collect();
            Ok(json!({ "devices": devices }))
        }
        "unpair" => {
            let a = agent()?;
            let peer = one_device(&a, text(args, "id")?)?;
            a.remove_device(&peer).map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "primary" => {
            let a = agent()?;
            let id = match args.get("id").and_then(Value::as_str) {
                Some(id) => Some(one_device(&a, id)?.to_hex()),
                None => None,
            };
            a.settings.lock().unwrap().primary_device = id;
            a.save_settings().map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "pair" => {
            let a = agent()?;
            let name = pair::pair(&a, text(args, "code")?)?;
            Ok(json!({ "paired": name }))
        }
        "pair_show" => pair_show(),
        "pair_status" => Ok(match &*SHOWING.lock().unwrap() {
            None => json!({ "state": "idle" }),
            Some(s) => match &*s.outcome.lock().unwrap() {
                None => json!({ "state": "waiting" }),
                Some(Ok(name)) => json!({ "state": "paired", "paired": name }),
                Some(Err(e)) => json!({ "state": "failed", "error": e }),
            },
        }),
        "pair_cancel" => {
            if let Some(s) = &*SHOWING.lock().unwrap() {
                s.cancel.store(true, Ordering::Relaxed);
            }
            Ok(json!({}))
        }
        "serve" => serve(),
        "serving" => Ok(match &*SERVING.lock().unwrap() {
            None => json!({ "state": "off" }),
            Some(Ok(())) => json!({ "state": "serving" }),
            Some(Err(e)) => json!({ "state": "stopped", "reason": e }),
        }),
        "extra_games" => extra_games(args),
        "rescan" => Ok(json!({ "changes": agent()?.rescan() })),
        "scan" => {
            let max_age = args.get("max_age_s").and_then(Value::as_u64).unwrap_or(0);
            serde_json::to_value(agent()?.fresh_scan(max_age)).map_err(|e| e.to_string())
        }
        "library" => serde_json::to_value(&agent()?.library_now().games).map_err(|e| e.to_string()),
        "set_mark" => {
            let changed = agent()?.set_mark(text(args, "game")?, text(args, "field")?, args.get("value").cloned().unwrap_or(Value::Null));
            Ok(json!({ "changed": changed }))
        }
        "saves" => {
            let a = agent()?;
            let game =
                GameRef { key: text(args, "key")?.to_string(), title: args.get("title").and_then(Value::as_str).unwrap_or("").to_string() };
            let fetch = args.get("fetch").and_then(Value::as_bool).unwrap_or(false);
            match a.save_lookup(&game, fetch) {
                None => Ok(json!({ "patterns": null, "files": [] })),
                Some((spec, roots, _)) => {
                    let files: Vec<Value> =
                        saves::collect(&spec, &roots).into_iter().map(|(name, path)| json!({ "name": name, "path": path })).collect();
                    Ok(json!({ "patterns": spec.patterns, "files": files }))
                }
            }
        }
        other => Err(format!("unknown operation {other}")),
    }
}

/// The paired device whose id starts with [`id`].
fn one_device(a: &Agent, id: &str) -> Result<PeerId, String> {
    let id = id.to_lowercase();
    let matches: Vec<String> = a.devices_now().into_iter().filter(|d| d.id.starts_with(&id)).map(|d| d.id).collect();
    match matches.as_slice() {
        [one] => PeerId::from_hex(one).map_err(|e| e.to_string()),
        [] => Err("No paired handheld has that id.".into()),
        _ => Err("More than one paired handheld starts with that; give more of the id.".into()),
    }
}

/// Changes the settings named in [`args`]; the others keep their values.
fn settings_set(args: &Value) -> Result<Value, String> {
    let a = agent()?;
    let Some(changes) = args.as_object() else { return Err("settings_set takes an object".into()) };
    let mut current = serde_json::to_value(&*a.settings.lock().unwrap()).map_err(|e| e.to_string())?;
    for (k, v) in changes {
        current[k] = v.clone();
    }
    let settings: Settings = serde_json::from_value(current).map_err(|e| format!("those settings are not valid: {e}"))?;
    *a.settings.lock().unwrap() = settings;
    a.save_settings().map_err(|e| e.to_string())?;
    Ok(json!({}))
}

/// Starts the agent's service (LAN sessions, discovery, WireGuard,
/// rendezvous, the cloud share) on threads of its own. If another agent
/// already listens on this computer, that one serves and this says so.
fn serve() -> Result<Value, String> {
    let a = agent()?;
    let mut serving = SERVING.lock().unwrap();
    if let Some(Ok(())) = &*serving {
        return Ok(json!({ "state": "serving" }));
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = droidtop_agent_service::serve::run(a);
        let stopped = match result {
            Ok(()) => "the agent stopped".to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                "another droidtop agent already serves this computer (droidtop-agent or another Pythia)".to_string()
            }
            Err(e) => format!("the agent could not listen: {e}"),
        };
        let _ = tx.send(stopped.clone());
        *SERVING.lock().unwrap() = Some(Err(stopped));
    });
    // A listener that cannot bind fails at once; one that binds serves until the process ends.
    match rx.recv_timeout(Duration::from_millis(1500)) {
        Ok(why) => {
            *serving = Some(Err(why.clone()));
            Err(why)
        }
        Err(_) => {
            *serving = Some(Ok(()));
            Ok(json!({ "state": "serving" }))
        }
    }
}

fn pair_show() -> Result<Value, String> {
    let a = agent()?;
    if let Some(s) = &*SHOWING.lock().unwrap() {
        s.cancel.store(true, Ordering::Relaxed);
    }
    let showing = pair::listen()?;
    let reply = json!({
        "code": showing.code,
        "port": showing.port,
        "addresses": showing.addresses,
        "places": showing.places.iter().map(|p| json!({ "label": p.label, "address": p.address })).collect::<Vec<_>>(),
        "invite": showing.invite(&a),
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(Mutex::new(None));
    {
        let (cancel, outcome) = (cancel.clone(), outcome.clone());
        thread::spawn(move || {
            let result = pair::wait(&a, showing, &cancel);
            *outcome.lock().unwrap() = Some(result);
        });
    }
    *SHOWING.lock().unwrap() = Some(Showing { cancel, outcome });
    Ok(reply)
}

/// A game the desktop manager tracks, as it hands it over.
#[derive(Deserialize)]
struct Extra {
    #[serde(flatten)]
    game: ScannedGame,
    #[serde(default)]
    base: Option<PathBuf>,
}

/// The games the desktop manager itself tracks, joined into every scan from
/// now on (docs/DESIGN.md section 16); rescans when asked.
fn extra_games(args: &Value) -> Result<Value, String> {
    let a = agent()?;
    let list: Vec<Extra> = serde_json::from_value(args.get("games").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
    let found: Vec<Found> = list.into_iter().map(|e| Found { game: e.game, base: e.base, prefix: None, engine: None }).collect();
    let count = found.len();
    *a.extra.lock().unwrap() = found;
    let changes = if args.get("rescan").and_then(Value::as_bool).unwrap_or(true) { a.rescan() } else { 0 };
    Ok(json!({ "games": count, "changes": changes }))
}
