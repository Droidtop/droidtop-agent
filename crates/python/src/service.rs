//! `droidtop_agent.service`: serving paired handhelds from this process
//! (droidtop-agent-service), the same service as `droidtop-agent run`.

use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;

use pyo3::prelude::*;
use serde_json::json;

use crate::{agent, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    m.add_function(wrap_pyfunction!(serving, m)?)?;
    m.add_function(wrap_pyfunction!(events, m)?)?;
    Ok(())
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
        let mut serving = SERVING.lock().unwrap();
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
    })
}

/// Whether this process serves: `state` is `off` (never asked), `serving`,
/// or `stopped` with the `reason`.
#[pyfunction]
fn serving(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        Ok(match &*SERVING.lock().unwrap() {
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
