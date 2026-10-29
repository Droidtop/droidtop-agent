//! The paired devices and what this computer gives them and takes from them: `devices`, `unpair`, `id`, `name`,
//! `offer`, `offered`, `accept`, `accepted`, `play-on`, `launch`.

use std::sync::Arc;

use droidtop_agent_core::keys::{short, PeerId};
use droidtop_agent_core::proto::{GameRef, Launch};
use droidtop_agent_core::sync::LockExt;
use droidtop_agent_service::Agent;

use super::{change, usage};

pub fn devices(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(usage("devices"));
    }
    let devices = agent.store.devices_now();
    if devices.is_empty() {
        println!("No handheld is paired. On droidtop open Settings > Computers > Pair a computer, then run: droidtop-agent pair <code>");
    }
    for d in devices.iter() {
        let seen =
            if d.last_seen_ms > 0 { format!(", last seen at {}", d.last_address.clone().unwrap_or_default()) } else { String::new() };
        println!("{}  {}{seen}", &d.id[..16], d.name);
    }
    Ok(())
}

/// The one paired device whose id starts with [`id`], as its full id and name.
fn one(agent: &Agent, id: &str, what: &str) -> Result<(String, String), String> {
    let wanted = id.to_lowercase();
    let matches: Vec<_> = agent.store.devices_now().into_iter().filter(|d| d.id.starts_with(&wanted)).collect();
    match matches.as_slice() {
        [one] => Ok((one.id.clone(), one.name.clone())),
        [] => Err(format!("No paired {what} has that id.")),
        _ => Err(format!("More than one paired {what} starts with that; give more of the id.")),
    }
}

fn one_peer(agent: &Agent, id: &str) -> Result<PeerId, String> {
    PeerId::from_hex(&one(agent, id, "device")?.0).map_err(|e| e.to_string())
}

pub fn unpair(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let [id] = args else { return Err(usage("unpair")) };
    let peer = PeerId::from_hex(&one(agent, id, "handheld")?.0).map_err(|e| e.to_string())?;
    agent.store.remove_device(&peer).map_err(|e| e.to_string())?;
    println!("Forgot {}.", short(&peer));
    Ok(())
}

pub fn id(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(usage("id"));
    }
    println!("{}\n{}", agent.store.name(), agent.store.peer_id());
    Ok(())
}

pub fn name(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    if args.is_empty() {
        return Err(usage("name"));
    }
    change(agent, |s| s.name = Some(args.join(" ")))
}

pub fn offer(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            let devices = agent.store.devices_now();
            if devices.is_empty() {
                println!("No device is paired.");
            }
            let launch = agent.store.settings.locked().launch_devices.clone();
            for d in &devices {
                let offer = PeerId::from_hex(&d.id).map(|p| agent.store.offers_to(&p)).unwrap_or_default();
                let given: Vec<&str> = [("library", offer.library), ("saves", offer.saves), ("starting games", launch.contains(&d.id))]
                    .into_iter()
                    .filter(|(_, on)| *on)
                    .map(|(what, _)| what)
                    .collect();
                let given = if given.is_empty() { "nothing".to_string() } else { given.join(", ") };
                println!("{}  {}  offered: {given}", &d.id[..16], d.name);
            }
            Ok(())
        }
        [id, "launch", on @ ("on" | "off")] => launch_switch(agent, id, *on == "on"),
        [id, what @ ("library" | "saves"), on @ ("on" | "off")] => {
            let (full, name) = one(agent, id, "device")?;
            let on = *on == "on";
            if *what == "library" {
                agent.store.set_offer(&full, Some(on), None);
            } else {
                agent.store.set_offer(&full, None, Some(on));
            }
            agent.store.save_settings().map_err(|e| e.to_string())?;
            let offer = PeerId::from_hex(&full).map(|p| agent.store.offers_to(&p)).unwrap_or_default();
            let said = match (offer.library, offer.saves) {
                (true, true) => "its library and its saves",
                (true, false) => "its library",
                _ => "nothing of its library or saves",
            };
            println!("This computer now offers {name} {said}.");
            Ok(())
        }
        _ => Err(usage("offer")),
    }
}

/// The offered game [`key`] (or the start of its title) on [`peer`].
fn offered_game(agent: &Agent, peer: &PeerId, key: &str) -> Result<GameRef, String> {
    let seen = agent.browse(peer)?;
    let wanted = key.to_lowercase();
    let game = seen
        .games
        .iter()
        .find(|g| g.key == key)
        .or_else(|| seen.games.iter().find(|g| g.title.to_lowercase().starts_with(&wanted)))
        .ok_or_else(|| format!("{} offers no game {key}.", seen.name))?;
    Ok(GameRef { key: game.key.clone(), title: game.title.clone() })
}

pub fn offered(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let [id] = args else { return Err(usage("offered")) };
    let peer = one_peer(agent, id)?;
    let seen = agent.browse(&peer)?;
    let given: Vec<&str> = [("its library", seen.offered.library), ("its saves", seen.offered.saves)]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(what, _)| what)
        .collect();
    println!("{} offers this computer {}.", seen.name, if given.is_empty() { "nothing".to_string() } else { given.join(" and ") });
    for g in &seen.games {
        let version = g.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
        println!("{:<40} {:<36}{version}{}", g.title, g.key, if g.accepted { "  (accepted)" } else { "" });
    }
    Ok(())
}

pub fn accept(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let [id, key] = args else { return Err(usage("accept")) };
    let peer = one_peer(agent, id)?;
    let game = offered_game(agent, &peer, key)?;
    let mut shown = 0u64;
    let accepted = agent.accept(&peer, &game, &mut |done: u64, total: u64| {
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        if percent >= shown + 10 || done == total {
            shown = percent;
            println!("{percent}%");
        }
    })?;
    println!("Copied {} into {}.", accepted.title, accepted.folder.display());
    Ok(())
}

pub fn accepted(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(usage("accepted"));
    }
    let all = agent.accepted();
    if all.is_empty() {
        println!("No game was accepted from another device.");
    }
    for a in all {
        let from = PeerId::from_hex(&a.from).map(|p| agent.store.device_name(&p)).unwrap_or(a.from.clone());
        println!("{}  from {from}  {}", a.title, a.folder.display());
    }
    Ok(())
}

pub fn play_on(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let [id, key] = args else { return Err(usage("play-on")) };
    let peer = one_peer(agent, id)?;
    let game = offered_game(agent, &peer, key)?;
    let said = match agent.play_on(&peer, &game)? {
        Launch::Started { stream: Some(s), .. } => format!("Started; it streams through {s}."),
        Launch::Started { stream: None, .. } => "Started.".into(),
        Launch::NotAllowed => "That device does not let this computer start games there.".into(),
        Launch::UnknownGame => "That device does not have that game installed.".into(),
        Launch::SteamNotRunning => "Steam is not running on that device.".into(),
        Launch::Failed { message } => format!("It could not start: {message}"),
    };
    println!("{said}");
    Ok(())
}

pub fn launch(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            let allowed = agent.store.settings.locked().launch_devices.clone();
            let devices = agent.store.devices_now();
            if devices.is_empty() {
                println!("No handheld is paired.");
            }
            for d in &devices {
                println!(
                    "{}  {}  {}",
                    &d.id[..16],
                    d.name,
                    if allowed.contains(&d.id) { "may start games" } else { "may not start games" }
                );
            }
            Ok(())
        }
        [verb @ ("allow" | "deny"), id] => launch_switch(agent, id, *verb == "allow"),
        _ => Err(usage("launch")),
    }
}

fn launch_switch(agent: &Agent, id: &str, allow: bool) -> Result<(), String> {
    let (full, name) = one(agent, id, "handheld")?;
    change(agent, |s| {
        s.launch_devices.retain(|d| *d != full);
        if allow {
            s.launch_devices.push(full.clone());
        }
    })?;
    println!("{name} {} start games on this computer.", if allow { "may now" } else { "may no longer" });
    Ok(())
}
