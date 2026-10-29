//! How this computer is reached and what it keeps in step: `run`, `share`, `contexts`, `endpoint`, `rendezvous`, `syncthing`,
//! `autostart`.

use std::path::PathBuf;
use std::sync::Arc;

use droidtop_agent_contexts as contexts;
use droidtop_agent_core::sync::LockExt;
use droidtop_agent_service::{autostart, host, rendezvous as service_rendezvous, serve, syncthing as service_syncthing, Agent};
use droidtop_agent_store::read_json;

use super::{change, usage};

pub fn run(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(usage("run"));
    }
    serve::run(agent.clone()).map_err(|e| e.to_string())
}

pub fn share(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            match &agent.store.settings.locked().share {
                Some(s) => println!("{}", s.display()),
                None => println!("No share is set."),
            }
            Ok(())
        }
        ["set", rest @ ..] if !rest.is_empty() => {
            let path = PathBuf::from(rest.join(" "));
            if !path.is_dir() {
                return Err(format!("{} is not a folder.", path.display()));
            }
            change(agent, |s| s.share = Some(path))
        }
        ["clear"] => change(agent, |s| s.share = None),
        _ => Err(usage("share")),
    }
}

pub fn contexts(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            let adapters = agent.store.settings.locked().adapters.clone();
            if adapters.is_empty() && contexts::adapter_offers(&agent.store).is_empty() {
                println!("No context adapter is added. A plugin that syncs with a program here publishes one: droidtop-agent contexts add <program>");
            }
            for id in adapters.keys() {
                let state = match contexts::adapter(&agent.store, id).map(|a| a.pull()) {
                    Some(Ok(records)) => format!("{} records", records.len()),
                    Some(Err(e)) => e,
                    None => "not available".into(),
                };
                println!("{id}: {} ({state})", adapters[id].display());
            }
            for (id, offer) in contexts::adapter_offers(&agent.store) {
                println!("{id}: offered by the plugin {}, waiting for: droidtop-agent contexts approve {id}", offer.plugin);
            }
            Ok(())
        }
        ["add", rest @ ..] if !rest.is_empty() => {
            let given = rest.join(" ");
            let program = std::fs::canonicalize(&given).map_err(|e| format!("{given}: {e}"))?;
            let (id, what) = contexts::describe(&program)?;
            change(agent, |s| {
                s.adapters.insert(id.clone(), program);
            })?;
            println!("Added the {id} context: {what}");
            Ok(())
        }
        ["publish", id, file] => std::fs::read(file)
            .map_err(|e| format!("{file}: {e}"))
            .and_then(|bytes| contexts::publish(&agent.store, id, &bytes))
            .map(|generation| println!("Published the {id} copy (generation {generation}); your handhelds read it at their next sync.")),
        ["approve", id] => contexts::approve(&agent.store, id).map(|line| println!("{line}")),
        ["remove", id] => {
            let removed = agent.store.settings.locked().adapters.remove(*id).is_some();
            agent
                .store
                .save_settings()
                .map_err(|e| e.to_string())
                .map(|()| println!("{}", if removed { "Removed." } else { "No adapter serves that context." }))
        }
        _ => Err(usage("contexts")),
    }
}

pub fn endpoint(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            for e in host::endpoints(agent.store.settings.locked().public_endpoint.as_deref()) {
                println!("{e}");
            }
            Ok(())
        }
        ["set", at] => match at.parse::<std::net::SocketAddr>() {
            Ok(_) => change(agent, |s| s.public_endpoint = Some(at.to_string())),
            Err(_) => Err(format!("{at} is not an address and port, such as 203.0.113.7:47611.")),
        },
        ["clear"] => change(agent, |s| s.public_endpoint = None),
        _ => Err(usage("endpoint")),
    }
}

pub fn rendezvous(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            let s = agent.store.settings.locked().clone();
            println!("Rendezvous: {}", if s.rendezvous { "on" } else { "off" });
            println!("Discovery servers: {}", s.discovery_servers.join(" "));
            println!("STUN servers: {}", s.stun_servers.join(" "));
            if let Ok(cert) = droidtop_agent_core::rendezvous::certificate(&agent.store.key) {
                println!("This computer's discovery ID: {}", cert.device_id());
            }
            match read_json::<service_rendezvous::Status>(&agent.store.dirs.rendezvous()) {
                Ok(st) if st.disco.is_some() => println!("{st:#?}"),
                _ => println!("No rendezvous state yet: it is kept while droidtop-agent runs."),
            }
            Ok(())
        }
        [on @ ("on" | "off")] => change(agent, |s| s.rendezvous = *on == "on"),
        ["servers", list @ ..] if !list.is_empty() => {
            match list.iter().find(|s| **s != "default" && droidtop_agent_core::rendezvous::Server::parse(s).is_none()) {
                Some(bad) => Err(format!("{bad} is not an https discovery server address (or default).")),
                None => change(agent, |s| s.discovery_servers = list.iter().map(|s| s.to_string()).collect()),
            }
        }
        ["stun", list @ ..] if !list.is_empty() => change(agent, |s| s.stun_servers = list.iter().map(|s| s.to_string()).collect()),
        _ => Err(usage("rendezvous")),
    }
}

pub fn syncthing(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            let s = agent.store.settings.locked().clone();
            println!("Syncthing with the paired devices: {}", if s.syncthing { "on" } else { "off" });
            match service_syncthing::home() {
                Some(home) => match service_syncthing::read_config(&home) {
                    Ok(c) => println!("Syncthing in {}: device ID {}", home.display(), c.device_id),
                    Err(e) => println!("Syncthing in {}: {e}", home.display()),
                },
                None => println!("This system has no folder for Syncthing."),
            }
            match s.announce.get(service_syncthing::PLUGIN) {
                Some(v) => println!("Announced to the paired devices: {v}"),
                None => println!("Nothing announced to the paired devices yet: that happens while droidtop-agent runs."),
            }
            Ok(())
        }
        [on @ ("on" | "off")] => change(agent, |s| s.syncthing = *on == "on"),
        _ => Err(usage("syncthing")),
    }
}

pub fn autostart(_agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            println!("Starting when you sign in: {}", if autostart::enabled() { "on" } else { "off" });
            Ok(())
        }
        [on @ ("on" | "off")] => autostart::set(*on == "on").map(|said| println!("{said}")),
        _ => Err(usage("autostart")),
    }
}
