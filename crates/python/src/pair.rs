//! `droidtop_agent.pair`: pairing a handheld from this computer
//! (droidtop-agent-pair), the way `droidtop-agent`'s window does.

use droidtop_agent_core::sync::LockExt;
use std::sync::{Arc, Mutex};
use std::thread;

use pyo3::prelude::*;
use serde_json::json;

use crate::{agent, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(pair, m)?)?;
    m.add_function(wrap_pyfunction!(show, m)?)?;
    m.add_function(wrap_pyfunction!(status, m)?)?;
    m.add_function(wrap_pyfunction!(cancel, m)?)?;
    m.add_function(wrap_pyfunction!(places, m)?)?;
    Ok(())
}

/// This computer showing a pairing code: the outcome once there is one.
struct Showing {
    cancel: Arc<droidtop_agent_core::accept::Cancel>,
    outcome: Arc<Mutex<Option<Result<String, String>>>>,
}

static SHOWING: Mutex<Option<Showing>> = Mutex::new(None);

/// Pairs with a handheld that shows `code` (droidtop: Settings > Computers >
/// Pair a computer): the 6 digits, or the text of its QR code. Waits for the
/// exchange (seconds); returns the handheld's name.
#[pyfunction]
fn pair(py: Python<'_>, code: String) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        Ok(json!(droidtop_agent_pair::pair(&a.store, &code)?))
    })
}

/// This computer shows a code for a handheld to type (droidtop: Pair a
/// computer > Use a code from the computer), for 10 minutes: `code`, `port`,
/// `places` (every address a handheld can use, each with its `label`),
/// `invite` (the text a QR code of the invitation carries) and `seconds`
/// left. Ask `status()` for the outcome; a code shown before is stopped.
#[pyfunction]
fn show(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        let a = agent()?;
        if let Some(s) = &*SHOWING.locked() {
            s.cancel.cancel();
        }
        let showing = droidtop_agent_pair::listen()?;
        let reply = json!({
            "code": showing.code,
            "port": showing.port,
            "places": showing.places.iter().map(|p| json!({ "label": p.label, "address": p.address })).collect::<Vec<_>>(),
            "invite": showing.invite(&a.store),
            "seconds": showing.until.saturating_duration_since(std::time::Instant::now()).as_secs(),
        });
        let cancel = Arc::new(droidtop_agent_core::accept::Cancel::new());
        let outcome = Arc::new(Mutex::new(None));
        {
            let (cancel, outcome) = (cancel.clone(), outcome.clone());
            thread::spawn(move || {
                let result = droidtop_agent_pair::wait(&a.store, showing, &cancel);
                *outcome.locked() = Some(result);
            });
        }
        *SHOWING.locked() = Some(Showing { cancel, outcome });
        Ok(reply)
    })
}

/// How the code `show()` shows is doing: `state` is `idle` (none shown),
/// `waiting`, `paired` (with the handheld's name as `paired`) or `failed`
/// (with `error`).
#[pyfunction]
fn status(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        Ok(match &*SHOWING.locked() {
            None => json!({ "state": "idle" }),
            Some(s) => match &*s.outcome.locked() {
                None => json!({ "state": "waiting" }),
                Some(Ok(name)) => json!({ "state": "paired", "paired": name }),
                Some(Err(e)) => json!({ "state": "failed", "error": e }),
            },
        })
    })
}

/// Stops showing a code.
#[pyfunction]
fn cancel(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        if let Some(s) = &*SHOWING.locked() {
            s.cancel.cancel();
        }
        Ok(json!(null))
    })
}

/// Every address of this computer a handheld can use with the agent's port,
/// each with its `label` ("Local network", "Tailscale", "IPv6"...).
#[pyfunction]
fn places(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        let list: Vec<_> = droidtop_agent_pair::places(droidtop_agent_core::PORT)
            .into_iter()
            .map(|p| json!({ "label": p.label, "address": p.address }))
            .collect();
        Ok(json!(list))
    })
}
