//! The computer's droidtop agent as a Python extension module,
//! `droidtop_agent`. A desktop manager that loads it (Pythia) is this
//! computer's agent while it runs: the same identity, pairings, library,
//! saves, scanner and service as `droidtop-agent run`, in its own process
//! and over the same files, so only one of the two runs on a computer and
//! either can take over from the other (docs/DESIGN.md section 16).
//!
//! One extension module with one submodule per crate of the agent:
//! `droidtop_agent.store`, `.pair`, `.scan`, `.saves`, `.remote` and `.service`. One
//! module and not one per crate: each extension module links its own copy of
//! the Rust code, and one process must hold one agent.
//!
//! Functions take keyword arguments and return dicts, lists and plain
//! values; a refusal raises `droidtop_agent.AgentError` with words a screen
//! can show. Every call releases the interpreter while Rust works, so a slow
//! one (a scan, a pairing wait) never stops Python's other threads.

// PyO3 0.22's #[pyfunction] expansion converts a PyErr into itself, which
// clippy calls a useless conversion; the code is PyO3's, not ours.
#![allow(clippy::useless_conversion)]

use droidtop_agent_core::sync::LockExt;
use std::sync::{Arc, Mutex, OnceLock};

use droidtop_agent_service::Agent;
use pyo3::prelude::*;
use serde_json::Value;

mod pair;
mod remote;
mod saves;
mod scan;
mod service;
mod store;

// PyO3 0.22's create_exception! tests a `gil-refs` feature this crate does not
// declare, which rustc's cfg check reports.
#[allow(unexpected_cfgs)]
mod error {
    pyo3::create_exception!(
        droidtop_agent,
        AgentError,
        pyo3::exceptions::PyException,
        "The agent refused a call; the message is in words a screen can show."
    );
}
pub(crate) use error::AgentError;

static AGENT: OnceLock<Arc<Agent>> = OnceLock::new();
static OPENING: Mutex<()> = Mutex::new(());

/// The agent over this computer's files, opened on first use.
pub(crate) fn agent() -> Result<Arc<Agent>, String> {
    if let Some(a) = AGENT.get() {
        return Ok(a.clone());
    }
    let _one = OPENING.locked();
    if let Some(a) = AGENT.get() {
        return Ok(a.clone());
    }
    let opened = Arc::new(Agent::open().map_err(|e| format!("the agent could not open its files: {e}"))?);
    Ok(AGENT.get_or_init(|| opened).clone())
}

/// Runs [`work`] without the interpreter and hands its result to Python.
pub(crate) fn run(py: Python<'_>, work: impl FnOnce() -> Result<Value, String> + Send) -> PyResult<PyObject> {
    match py.allow_threads(work) {
        Ok(value) => to_py(py, &value),
        Err(message) => Err(AgentError::new_err(message)),
    }
}

/// A JSON value as Python's own objects.
fn to_py(py: Python<'_>, value: &Value) -> PyResult<PyObject> {
    Ok(py.import_bound("json")?.call_method1("loads", (value.to_string(),))?.unbind())
}

/// A Python object (dicts, lists and plain values) as a JSON value.
pub(crate) fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let text: String = obj.py().import_bound("json")?.call_method1("dumps", (obj,))?.extract()?;
    serde_json::from_str(&text).map_err(|e| AgentError::new_err(e.to_string()))
}

/// Adds submodule [`name`], filled by [`fill`], as `droidtop_agent.<name>`,
/// importable as such.
fn submodule(parent: &Bound<'_, PyModule>, name: &str, fill: fn(&Bound<'_, PyModule>) -> PyResult<()>) -> PyResult<()> {
    let py = parent.py();
    let full = format!("droidtop_agent.{name}");
    let module = PyModule::new_bound(py, name)?;
    fill(&module)?;
    // Added under its short name first: add_submodule names the attribute after `__name__`.
    parent.add_submodule(&module)?;
    module.setattr("__name__", &full)?;
    py.import_bound("sys")?.getattr("modules")?.set_item(full, &module)?;
    Ok(())
}

#[pymodule]
fn droidtop_agent(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("PROTOCOL_VERSION", droidtop_agent_core::PROTOCOL_VERSION)?;
    m.add("AgentError", m.py().get_type_bound::<AgentError>())?;
    submodule(m, "store", store::fill)?;
    submodule(m, "pair", pair::fill)?;
    submodule(m, "scan", scan::fill)?;
    submodule(m, "saves", saves::fill)?;
    submodule(m, "remote", remote::fill)?;
    submodule(m, "service", service::fill)?;
    Ok(())
}
