//! `droidtop_agent.remote`: the side that receives an offer (docs/DESIGN.md
//! section 20). This computer asks a paired device what it offers, copies a
//! game it offers, or asks it to play one; the device decides what it
//! answers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use droidtop_agent_core::proto::GameRef;
use pyo3::prelude::*;
use serde_json::json;

use crate::store::one_device;
use crate::{agent, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(offered, m)?)?;
    m.add_function(wrap_pyfunction!(accept, m)?)?;
    m.add_function(wrap_pyfunction!(play_on, m)?)?;
    m.add_function(wrap_pyfunction!(accepted, m)?)?;
    m.add_function(wrap_pyfunction!(set_keep_in_step, m)?)?;
    Ok(())
}

/// What a paired device (the start of its id is enough) offers this
/// computer: `name`, `offered` (`library`, `saves`, `launch`) and `games`,
/// each with `key`, `title`, `platform`, `version`, `size`, `marks` and
/// whether this computer `accepted` it before. Reaching the device can take
/// a few seconds; a device that cannot be reached raises AgentError.
#[pyfunction]
fn offered(py: Python<'_>, id: String) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let peer = one_device(&a.store, &id)?;
        serde_json::to_value(a.browse(&peer)?).map_err(|e| e.to_string())
    })
}

/// Copies the game [`key`] (titled [`title`]) that the device offers into
/// this computer's first game folder, as a new folder. How far it has come
/// is written to [`progress`], when given, as `{"done": .., "total": ..}`
/// at most twice a second. Returns what was accepted: `key`, `title`,
/// `from`, `folder`, `version`, `at_ms`, `keep_in_step`.
#[pyfunction]
#[pyo3(signature = (id, key, title, progress = None))]
fn accept(py: Python<'_>, id: String, key: String, title: String, progress: Option<PathBuf>) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let peer = one_device(&a.store, &id)?;
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut report = |done: u64, total: u64| {
            let Some(path) = &progress else { return };
            if done < total && last.elapsed() < Duration::from_millis(500) {
                return;
            }
            last = Instant::now();
            let _ = std::fs::write(path, json!({ "done": done, "total": total }).to_string());
        };
        serde_json::to_value(a.accept(&peer, &GameRef { key, title }, &mut report)?).map_err(|e| e.to_string())
    })
}

/// Asks the device to start the game [`key`] there and stream it. Returns
/// `{"result": ...}`: `started` (with `stream` when windowcast shows it),
/// `not_allowed`, `unknown_game`, `steam_not_running` or `failed` with a
/// `message`.
#[pyfunction]
fn play_on(py: Python<'_>, id: String, key: String, title: String) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let peer = one_device(&a.store, &id)?;
        serde_json::to_value(a.play_on(&peer, &GameRef { key, title })?).map_err(|e| e.to_string())
    })
}

/// The games this computer accepted from other devices, newest first.
#[pyfunction]
fn accepted(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || serde_json::to_value(agent()?.accepted()).map_err(|e| e.to_string()))
}

/// Keeps the game accepted into [`folder`] in step with the device it came
/// from, or stops; off until the person turns it on.
#[pyfunction]
fn set_keep_in_step(py: Python<'_>, folder: PathBuf, on: bool) -> PyResult<PyObject> {
    run(py, move || {
        agent()?.set_keep_in_step(&folder, on)?;
        Ok(json!(null))
    })
}
