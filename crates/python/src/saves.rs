//! `droidtop_agent.saves`: where games keep their saves on this computer and
//! the copies kept of them (droidtop-agent-saves).

use std::path::PathBuf;

use droidtop_agent_core::proto::GameRef;
use pyo3::prelude::*;
use serde_json::json;

use crate::{agent, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(lookup, m)?)?;
    m.add_function(wrap_pyfunction!(copies, m)?)?;
    m.add_function(wrap_pyfunction!(restore, m)?)?;
    Ok(())
}

/// Where a game (by `key`, or by `title` when the key is not found) keeps
/// its saves here: `patterns` (None when nothing is known) and the `files`
/// found now, each with its `name` and `path`. With `fetch`, the Ludusavi
/// manifest is fetched first when there is none yet. May rescan (slow).
#[pyfunction]
#[pyo3(signature = (key, title = String::new(), fetch = false))]
fn lookup(py: Python<'_>, key: String, title: String, fetch: bool) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        Ok(match a.save_lookup(&GameRef { key, title }, fetch) {
            None => json!({ "patterns": null, "files": [] }),
            Some((spec, roots, _)) => {
                let files: Vec<_> = droidtop_agent_core::saves::collect(&spec, &roots)
                    .into_iter()
                    .map(|(name, path)| json!({ "name": name, "path": path }))
                    .collect();
                json!({ "patterns": spec.patterns, "files": files })
            }
        })
    })
}

/// Every game's copies of saves kept on this computer: each game's `key`
/// (None when the library no longer has it), `title`, `dir` and `copies`,
/// each copy with its `device`, `stamp` (UTC), `files`, `path`, `newest`
/// (the newest from that device) and `reviewed`.
#[pyfunction]
fn copies(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        let a = agent()?;
        let titles = droidtop_agent_saves::library_titles(&a.store.library_now());
        serde_json::to_value(droidtop_agent_saves::copies::kept(&a.store.dirs.archive(), &titles)).map_err(|e| e.to_string())
    })
}

/// Puts the kept copy at `path` back where the game keeps its saves; what
/// is there now is kept first as this computer's own copy. Returns how many
/// files it put back. May rescan (slow).
#[pyfunction]
#[pyo3(signature = (key, path, title = String::new()))]
fn restore(py: Python<'_>, key: String, path: PathBuf, title: String) -> PyResult<PyObject> {
    run(py, move || Ok(json!(agent()?.restore_saves(&GameRef { key, title }, &path)?)))
}
