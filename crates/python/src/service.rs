//! `droidtop_agent.service`: serving paired handhelds from this process
//! (droidtop-agent-service), the same service as `droidtop-agent run`.

use droidtop_agent_core::sync::LockExt;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::IntoPyDict;
use serde_json::json;

use crate::{agent, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    m.add_function(wrap_pyfunction!(serving, m)?)?;
    m.add_function(wrap_pyfunction!(events, m)?)?;
    m.add_function(wrap_pyfunction!(set_launcher, m)?)?;
    Ok(())
}

/// The desktop manager's launcher as the agent's (droidtop-agent-service
/// launch::Launcher): a Python callable that starts one of the manager's own
/// games for a handheld.
struct PythonLauncher(PyObject);

impl droidtop_agent_service::launch::Launcher for PythonLauncher {
    fn launch(&self, game: &droidtop_agent_core::proto::GameRef) -> Option<droidtop_agent_core::proto::Launch> {
        Python::with_gil(|py| {
            let answer = match self.0.call_bound(py, (), Some(&[("key", &game.key), ("title", &game.title)].into_py_dict_bound(py))) {
                Ok(answer) => answer,
                Err(e) => {
                    return Some(droidtop_agent_core::proto::Launch::Failed {
                        message: format!("{} could not start it: {e}", crate::title()),
                    })
                }
            };
            if answer.is_none(py) {
                return None;
            }
            let value = match crate::from_py(answer.bind(py)) {
                Ok(value) => value,
                Err(e) => return Some(droidtop_agent_core::proto::Launch::Failed { message: e.to_string() }),
            };
            Some(serde_json::from_value(value.clone()).unwrap_or_else(|e| droidtop_agent_core::proto::Launch::Failed {
                message: format!("{} answered {value} ({e})", crate::title()),
            }))
        })
    }
}

/// Starts the desktop manager's own games for a handheld that may start games here:
/// `launcher(key=, title=)` is called for every key that is not Steam's
/// (on one of the service's threads) and returns None when the key is not
/// one of its games, else the outcome as the protocol has it:
/// `{"result": "started", "stream": "windowcast"}`, `{"result":
/// "unknown_game"}` or `{"result": "failed", "message": ...}`. A launcher
/// that raises is `failed` with its words. While one is set, the hello
/// states `launch_host`. None takes it away.
#[pyfunction]
#[pyo3(signature = (launcher))]
fn set_launcher(py: Python<'_>, launcher: Option<PyObject>) -> PyResult<PyObject> {
    let a = py.allow_threads(agent).map_err(crate::AgentError::new_err)?;
    let set = launcher.is_some();
    *a.launcher.lock().unwrap() =
        launcher.map(|l| std::sync::Arc::new(PythonLauncher(l)) as std::sync::Arc<dyn droidtop_agent_service::launch::Launcher>);
    run(py, move || Ok(json!({ "launcher": set })))
}

/// The service, once started: Ok while it serves, or why it stopped.
static SERVING: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// Starts serving paired handhelds (LAN sessions, discovery, WireGuard, the
/// rendezvous, the cloud share) on threads of the agent's own, until the
/// process ends. Only one agent serves a computer: when another one already
/// does (droidtop-agent, its window, or another Pythia), this raises
/// AgentError saying so and that one keeps serving, over the same files.
#[pyfunction]
fn serve(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        let a = agent()?;
        let mut serving = SERVING.locked();
        if let Some(Ok(())) = &*serving {
            return Ok(json!({ "state": "serving" }));
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let stopped = match droidtop_agent_service::serve::run(a) {
                Ok(()) => "the agent stopped".to_string(),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    "another droidtop agent already serves this computer (droidtop-agent or another Pythia)".to_string()
                }
                Err(e) => format!("the agent could not listen: {e}"),
            };
            let _ = tx.send(stopped.clone());
            *SERVING.locked() = Some(Err(stopped));
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
    })
}

/// Whether this process serves: `state` is `off` (never asked), `serving`,
/// or `stopped` with the `reason`.
#[pyfunction]
fn serving(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        Ok(match &*SERVING.locked() {
            None => json!({ "state": "off" }),
            Some(Ok(())) => json!({ "state": "serving" }),
            Some(Err(e)) => json!({ "state": "stopped", "reason": e }),
        })
    })
}

/// What the service did after event `since` (0: all it still keeps, the
/// last 500), oldest first: each with its `seq`, `ms` (when, since 1970),
/// `level` (`info` or `warn`), `kind` (`listening`, `session`, `launch`,
/// `saves`, `library`, `arrived`, `discovery`, `wireguard`, `rendezvous`,
/// `share` or `manager`), `message` (the line `droidtop-agent run` prints)
/// and what it is about (`peer`, `device`, `title`, `key`, `changes`...).
/// Pass the last `seq` read as `since` next time. With `wait` (seconds) it
/// waits that long for an event when there is none yet, without holding
/// the interpreter, so a thread can follow the service with
/// `while True: new = events(since=seen, wait=30)`.
#[pyfunction]
#[pyo3(signature = (since = 0, wait = 0.0))]
fn events(py: Python<'_>, since: u64, wait: f64) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let got = match Duration::try_from_secs_f64(wait) {
            Ok(wait) if !wait.is_zero() => a.events.wait_since(since, wait),
            _ => a.events.since(since),
        };
        serde_json::to_value(got).map_err(|e| e.to_string())
    })
}
