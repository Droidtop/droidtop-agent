//! `droidtop_agent.store`: this computer's agent state (droidtop-agent-store):
//! its identity, the paired handhelds, the settings and the library.

use droidtop_agent_core::keys::PeerId;
use droidtop_agent_store::{Settings, Store};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};

use crate::{agent, from_py, run};

pub fn fill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(identity, m)?)?;
    m.add_function(wrap_pyfunction!(settings, m)?)?;
    m.add_function(wrap_pyfunction!(set_settings, m)?)?;
    m.add_function(wrap_pyfunction!(devices, m)?)?;
    m.add_function(wrap_pyfunction!(unpair, m)?)?;
    m.add_function(wrap_pyfunction!(set_primary, m)?)?;
    m.add_function(wrap_pyfunction!(set_launch, m)?)?;
    m.add_function(wrap_pyfunction!(library, m)?)?;
    m.add_function(wrap_pyfunction!(set_mark, m)?)?;
    Ok(())
}

/// This computer as paired handhelds know it: `name` and `id`, and the
/// agent's `config` and `data` folders.
#[pyfunction]
fn identity(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        let a = agent()?;
        Ok(json!({ "name": a.store.name(), "id": a.store.peer_id().to_hex(), "config": a.store.dirs.config, "data": a.store.dirs.data }))
    })
}

/// The person's settings, as `droidtop-agent`'s window shows them.
#[pyfunction]
fn settings(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || serde_json::to_value(&*agent()?.store.settings.lock().unwrap()).map_err(|e| e.to_string()))
}

/// Changes the settings named as keyword arguments (`name="DESKTOP"`,
/// `game_folders=[...]`); the others keep their values.
#[pyfunction]
#[pyo3(signature = (**changes))]
fn set_settings(py: Python<'_>, changes: Option<&Bound<'_, PyDict>>) -> PyResult<PyObject> {
    let changes = match changes {
        Some(c) => from_py(c.as_any())?,
        None => json!({}),
    };
    run(py, move || {
        let a = agent()?;
        let mut current = serde_json::to_value(&*a.store.settings.lock().unwrap()).map_err(|e| e.to_string())?;
        for (k, v) in changes.as_object().into_iter().flatten() {
            current[k] = v.clone();
        }
        let settings: Settings = serde_json::from_value(current).map_err(|e| format!("those settings are not valid: {e}"))?;
        *a.store.settings.lock().unwrap() = settings;
        a.store.save_settings().map_err(|e| e.to_string())?;
        Ok(json!(null))
    })
}

/// The paired handhelds: each with `id`, `name`, `paired_ms`,
/// `last_seen_ms`, `last_address`, whether it is still `trusted`, whether
/// its saves win here (`primary`) and whether it may start games here
/// (`launch`).
#[pyfunction]
fn devices(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || {
        let a = agent()?;
        let (primary, launch) = {
            let s = a.store.settings.lock().unwrap();
            (s.primary_device.clone(), s.launch_devices.clone())
        };
        let devices: Vec<Value> = a
            .store
            .devices_now()
            .into_iter()
            .map(|d| {
                let trusted = PeerId::from_hex(&d.id).is_ok_and(|p| a.store.is_trusted(&p));
                let (is_primary, may_launch) = (primary.as_deref() == Some(d.id.as_str()), launch.contains(&d.id));
                let mut v = serde_json::to_value(&d).unwrap_or(Value::Null);
                v["trusted"] = json!(trusted);
                v["primary"] = json!(is_primary);
                v["launch"] = json!(may_launch);
                v
            })
            .collect();
        Ok(json!(devices))
    })
}

/// Forgets a paired handheld (the start of its id is enough): it can no
/// longer connect, to this program or to windowcast.
#[pyfunction]
fn unpair(py: Python<'_>, id: String) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let peer = one_device(&a.store, &id)?;
        a.store.remove_device(&peer).map_err(|e| e.to_string())?;
        Ok(json!(null))
    })
}

/// The handheld whose saves win here when both sides changed; with no id,
/// the newest copy wins.
#[pyfunction]
#[pyo3(signature = (id = None))]
fn set_primary(py: Python<'_>, id: Option<String>) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let id = match id {
            Some(id) => Some(one_device(&a.store, &id)?.to_hex()),
            None => None,
        };
        a.store.settings.lock().unwrap().primary_device = id;
        a.store.save_settings().map_err(|e| e.to_string())?;
        Ok(json!(null))
    })
}

/// Whether a paired handheld may start games on this computer (to stream
/// them with Steam Link; docs/DESIGN.md section 17). Off until the person
/// turns it on for that handheld.
#[pyfunction]
fn set_launch(py: Python<'_>, id: String, allowed: bool) -> PyResult<PyObject> {
    run(py, move || {
        let a = agent()?;
        let id = one_device(&a.store, &id)?.to_hex();
        {
            let mut s = a.store.settings.lock().unwrap();
            s.launch_devices.retain(|d| *d != id);
            if allowed {
                s.launch_devices.push(id);
            }
        }
        a.store.save_settings().map_err(|e| e.to_string())?;
        Ok(json!(null))
    })
}

/// The library every paired device shares, by game key: each game's
/// installs per device, the person's marks and its versions.
#[pyfunction]
fn library(py: Python<'_>) -> PyResult<PyObject> {
    run(py, || serde_json::to_value(&agent()?.store.library_now().games).map_err(|e| e.to_string()))
}

/// Sets one of the person's marks on a game (`favourite`, `hidden`,
/// `completed`, `rating`, `collections`...): it travels to every paired
/// device like one made on a handheld. Returns whether it changed.
#[pyfunction]
fn set_mark(py: Python<'_>, game: String, field: String, value: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let value = from_py(value)?;
    run(py, move || Ok(json!(agent()?.store.set_mark(&game, &field, value))))
}

/// The paired device whose id starts with [`id`].
fn one_device(store: &Store, id: &str) -> Result<PeerId, String> {
    let id = id.to_lowercase();
    let matches: Vec<String> = store.devices_now().into_iter().filter(|d| d.id.starts_with(&id)).map(|d| d.id).collect();
    match matches.as_slice() {
        [one] => PeerId::from_hex(one).map_err(|e| e.to_string()),
        [] => Err("No paired handheld has that id.".into()),
        _ => Err("More than one paired handheld starts with that; give more of the id.".into()),
    }
}
