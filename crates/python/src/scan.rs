//! `droidtop_agent.scan`: the computer's scanner (droidtop-agent-scan), as
//! the running agent uses it, or on its own over the folders given.

use std::path::PathBuf;

use droidtop_agent_service::manager::ManagerGame;
use pyo3::prelude::*;
use serde_json::json;

use crate::{agent, from_py, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(scan, m)?)?;
    m.add_function(wrap_pyfunction!(rescan, m)?)?;
    m.add_function(wrap_pyfunction!(set_manager_games, m)?)?;
    m.add_function(wrap_pyfunction!(scan_with, m)?)?;
    Ok(())
}

/// The agent's last scan, made now when it is older than `max_age_s` (0:
/// always). `games` (each with its `game`, folder `base`, Wine `prefix` and
/// `engine`), `apps`, `programs`, `sources` (each with its count) and
/// `problems`. Slow when it scans: it walks the game folders.
#[pyfunction]
#[pyo3(signature = (max_age_s = 0))]
fn scan(py: Python<'_>, max_age_s: u64) -> PyResult<PyObject> {
    run(py, move || serde_json::to_value(agent()?.fresh_scan(max_age_s)).map_err(|e| e.to_string()))
}

/// Scans now and records what changed in the library; returns how many
/// changes it made. Slow: it walks the game folders.
#[pyfunction]
fn rescan(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || Ok(json!(agent()?.rescan())))
}

/// The games `manager` (a desktop manager such as Pythia) tracks itself,
/// each a library record (`key`, `title`, `platform`, `install`) with its
/// folder as `base`. They are kept and joined into every scan from now on,
/// whichever program serves, where the scanner did not find the game by key
/// or folder; they replace the ones handed over before. Rescans when
/// `rescan` (slow). Returns the number of games and of library changes.
#[pyfunction]
#[pyo3(signature = (manager, games, rescan = true))]
fn set_manager_games(py: Python<'_>, manager: String, games: &Bound<'_, PyAny>, rescan: bool) -> PyResult<PyObject> {
    let games = from_py(games)?;
    run(py, move || {
        let list: Vec<ManagerGame> = serde_json::from_value(games).map_err(|e| format!("those games are not valid: {e}"))?;
        let count = list.len();
        let a = agent()?;
        a.set_manager_games(&manager, list).map_err(|e| e.to_string())?;
        let changes = if rescan { a.rescan() } else { 0 };
        Ok(json!({ "games": count, "changes": changes }))
    })
}

/// The scanner on its own, over the folders given, without the agent's
/// state or settings: what `scan()` gives, nothing recorded. Slow.
#[pyfunction]
#[pyo3(signature = (game_folders = Vec::new(), rom_folders = Vec::new(), disabled_folders = Vec::new(), disabled_sources = Vec::new()))]
fn scan_with(
    py: Python<'_>,
    game_folders: Vec<PathBuf>,
    rom_folders: Vec<PathBuf>,
    disabled_folders: Vec<PathBuf>,
    disabled_sources: Vec<String>,
) -> PyResult<PyObject> {
    run(py, move || {
        let options = droidtop_agent_scan::Options { game_folders, rom_folders, disabled_folders, disabled_sources };
        serde_json::to_value(droidtop_agent_scan::scan(&options)).map_err(|e| e.to_string())
    })
}
