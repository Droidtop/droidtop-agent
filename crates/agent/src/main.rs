//! droidtop-agent: keeps this computer and droidtop on a handheld in step
//! (docs/DESIGN.md). Run without arguments it serves paired handhelds.

use std::path::PathBuf;
use std::sync::Arc;

use droidtop_agent_core::keys::{short, PeerId};
use droidtop_agent_core::library::title_key;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves;

mod contexts;
mod host;
mod ludusavi;
mod pair;
mod rendezvous;
mod scan;
mod serve;
mod share;
mod state;

use state::Agent;

const HELP: &str = "droidtop-agent: keeps this computer and droidtop on a handheld in step.

Usage:
  droidtop-agent [run]                 Serve paired handhelds (keep it running)
  droidtop-agent pair <code>           Pair with a handheld showing a pairing code
                                       (droidtop: Settings > Computers > Pair a computer).
                                       <code> is the 6 digits, or the text of its QR code.
  droidtop-agent pair                  Show this computer's address and a code to type on the handheld
                                       (droidtop: Pair a computer > Use a code from the computer),
                                       for a handheld this computer cannot reach
  droidtop-agent devices               List paired handhelds
  droidtop-agent unpair <id>           Forget a handheld (the start of its id is enough)
  droidtop-agent id                    Show this computer's name and id
  droidtop-agent name <name>           Set the name the handheld shows for this computer
  droidtop-agent scan [--json]         Show the games and apps found on this computer
  droidtop-agent folders               List game and ROM folders
  droidtop-agent folders add [--roms] <path>
  droidtop-agent folders remove <path>
  droidtop-agent saves <game>          Where a game keeps its saves here, and the files found
                                       (<game> is a key such as steam:440, or a title)
  droidtop-agent saves add <game> <template>...
                                       State a game's save locations yourself, e.g.
                                       saves add steam:440 \"<winDocuments>/My Games/TF2\"
  droidtop-agent saves remove <game>
  droidtop-agent manifest update       Fetch the Ludusavi manifest of save locations now
  droidtop-agent share [set <folder> | clear]
                                       A folder your own sync tool carries to the handheld,
                                       used when the two are never online together
  droidtop-agent contexts              The plugin contexts this computer can sync
  droidtop-agent contexts add <program>
                                       Add a plugin's context adapter (a program its plugin
                                       publishes, e.g. droidtop-agent-f95-adapter)
  droidtop-agent contexts remove <context>
  droidtop-agent contexts approve <context>
                                       Install the adapter a plugin on the handheld offers for
                                       <context> (asked for once; later versions follow)
  droidtop-agent endpoint [set <ip:port> | clear]
                                       Where this computer's WireGuard port (UDP 47611) answers from
                                       the internet, if you forwarded it on your router; the handheld
                                       uses it, and this computer's global IPv6 addresses, when away
  droidtop-agent rendezvous [on | off] Finding each other away from home the way Syncthing does: STUN for
                                       this computer's address, Syncthing's global discovery to announce it
                                       (addresses only; syncs go through the direct WireGuard tunnel)
  droidtop-agent rendezvous servers default | <url>...
                                       Discovery servers, in Syncthing's notation (default: Syncthing's)
  droidtop-agent rendezvous stun default | <host:port>...
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help" | "help")) {
        print!("{HELP}");
        return;
    }
    let agent = match Agent::open() {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("droidtop-agent could not start: {e}");
            std::process::exit(1);
        }
    };
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        [] | ["run"] => serve::run(agent).map_err(|e| e.to_string()),
        ["pair", rest @ ..] if !rest.is_empty() => pair::pair(&agent, &rest.join(" ")).map(|name| println!("Paired with {name}.")),
        ["pair"] => pair::show(&agent).map(|name| println!("Paired with {name}.")),
        ["devices"] => {
            devices(&agent);
            Ok(())
        }
        ["unpair", id] => unpair(&agent, id),
        ["id"] => {
            println!("{}\n{}", agent.name(), agent.peer_id());
            Ok(())
        }
        ["name", rest @ ..] if !rest.is_empty() => {
            agent.settings.lock().unwrap().name = Some(rest.join(" "));
            agent.save_settings().map_err(|e| e.to_string())
        }
        ["scan"] => scan(&agent, false),
        ["scan", "--json"] => scan(&agent, true),
        ["folders"] => {
            let s = agent.settings.lock().unwrap();
            s.game_folders.iter().for_each(|f| println!("games  {}", f.display()));
            s.rom_folders.iter().for_each(|f| println!("roms   {}", f.display()));
            Ok(())
        }
        ["folders", "add", "--roms", path] => folders(&agent, path, true, true),
        ["folders", "add", path] => folders(&agent, path, false, true),
        ["folders", "remove", path] => folders(&agent, path, false, false),
        ["saves", "add", game, templates @ ..] if !templates.is_empty() => saves_add(&agent, game, templates),
        ["saves", "remove", game] => {
            let removed = agent.saves.lock().unwrap().remove(&game_ref(game).key).is_some();
            agent
                .save_saves()
                .map_err(|e| e.to_string())
                .map(|()| println!("{}", if removed { "Removed." } else { "There was nothing set for that game." }))
        }
        ["saves", rest @ ..] if !rest.is_empty() => saves_show(&agent, &rest.join(" ")),
        ["manifest", "update"] => {
            ludusavi::update(&agent.dirs.data).map(|i| println!("The Ludusavi manifest has save locations for {} games.", i.games.len()))
        }
        ["share"] => {
            match &agent.settings.lock().unwrap().share {
                Some(s) => println!("{}", s.display()),
                None => println!("No share is set."),
            }
            Ok(())
        }
        ["share", "set", rest @ ..] if !rest.is_empty() => {
            let path = PathBuf::from(rest.join(" "));
            if !path.is_dir() {
                Err(format!("{} is not a folder.", path.display()))
            } else {
                agent.settings.lock().unwrap().share = Some(path);
                agent.save_settings().map_err(|e| e.to_string())
            }
        }
        ["share", "clear"] => {
            agent.settings.lock().unwrap().share = None;
            agent.save_settings().map_err(|e| e.to_string())
        }
        ["contexts"] => {
            let adapters = agent.settings.lock().unwrap().adapters.clone();
            if adapters.is_empty() && agent.adapter_offers().is_empty() {
                println!("No context adapter is added. A plugin that syncs with a program here publishes one: droidtop-agent contexts add <program>");
            }
            for id in adapters.keys() {
                let state = match agent.adapter(id).map(|a| a.pull()) {
                    Some(Ok(records)) => format!("{} records", records.len()),
                    Some(Err(e)) => e,
                    None => "not available".into(),
                };
                println!("{id}: {} ({state})", adapters[id].display());
            }
            for (id, offer) in agent.adapter_offers() {
                println!("{id}: offered by the plugin {}, waiting for: droidtop-agent contexts approve {id}", offer.plugin);
            }
            Ok(())
        }
        ["contexts", "add", rest @ ..] if !rest.is_empty() => {
            let given = rest.join(" ");
            std::fs::canonicalize(&given).map_err(|e| format!("{given}: {e}")).and_then(|program| {
                let (id, what) = contexts::describe(&program)?;
                agent.settings.lock().unwrap().adapters.insert(id.clone(), program);
                agent.save_settings().map_err(|e| e.to_string())?;
                println!("Added the {id} context: {what}");
                Ok(())
            })
        }
        ["contexts", "approve", id] => agent.approve(id).map(|line| println!("{line}")),
        ["contexts", "remove", id] => {
            let removed = agent.settings.lock().unwrap().adapters.remove(*id).is_some();
            agent
                .save_settings()
                .map_err(|e| e.to_string())
                .map(|()| println!("{}", if removed { "Removed." } else { "No adapter serves that context." }))
        }
        ["endpoint"] => {
            for e in host::endpoints(agent.settings.lock().unwrap().public_endpoint.as_deref()) {
                println!("{e}");
            }
            Ok(())
        }
        ["endpoint", "set", at] => match at.parse::<std::net::SocketAddr>() {
            Ok(_) => {
                agent.settings.lock().unwrap().public_endpoint = Some(at.to_string());
                agent.save_settings().map_err(|e| e.to_string())
            }
            Err(_) => Err(format!("{at} is not an address and port, such as 203.0.113.7:47611.")),
        },
        ["rendezvous"] => {
            let s = agent.settings.lock().unwrap().clone();
            println!("Rendezvous: {}", if s.rendezvous { "on" } else { "off" });
            println!("Discovery servers: {}", s.discovery_servers.join(" "));
            println!("STUN servers: {}", s.stun_servers.join(" "));
            if let Ok(cert) = droidtop_agent_core::rendezvous::certificate(&agent.key) {
                println!("This computer's discovery ID: {}", cert.device_id());
            }
            match state::read_json::<rendezvous::Status>(&agent.dirs.rendezvous()) {
                Ok(st) if st.disco.is_some() => println!("{st:#?}"),
                _ => println!("No rendezvous state yet: it is kept while droidtop-agent runs."),
            }
            Ok(())
        }
        ["rendezvous", on @ ("on" | "off")] => {
            agent.settings.lock().unwrap().rendezvous = *on == "on";
            agent.save_settings().map_err(|e| e.to_string())
        }
        ["rendezvous", "servers", list @ ..] if !list.is_empty() => {
            match list.iter().find(|s| **s != "default" && droidtop_agent_core::rendezvous::Server::parse(s).is_none()) {
                Some(bad) => Err(format!("{bad} is not an https discovery server address (or default).")),
                None => {
                    agent.settings.lock().unwrap().discovery_servers = list.iter().map(|s| s.to_string()).collect();
                    agent.save_settings().map_err(|e| e.to_string())
                }
            }
        }
        ["rendezvous", "stun", list @ ..] if !list.is_empty() => {
            agent.settings.lock().unwrap().stun_servers = list.iter().map(|s| s.to_string()).collect();
            agent.save_settings().map_err(|e| e.to_string())
        }
        ["endpoint", "clear"] => {
            agent.settings.lock().unwrap().public_endpoint = None;
            agent.save_settings().map_err(|e| e.to_string())
        }
        _ => Err(format!("Unknown command.\n\n{HELP}")),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn devices(agent: &Agent) {
    let devices = agent.devices.lock().unwrap();
    if devices.is_empty() {
        println!("No handheld is paired. On droidtop open Settings > Computers > Pair a computer, then run: droidtop-agent pair <code>");
    }
    for d in devices.iter() {
        let seen =
            if d.last_seen_ms > 0 { format!(", last seen at {}", d.last_address.clone().unwrap_or_default()) } else { String::new() };
        println!("{}  {}{seen}", &d.id[..16], d.name);
    }
}

fn unpair(agent: &Agent, id: &str) -> Result<(), String> {
    let matches: Vec<String> =
        agent.devices.lock().unwrap().iter().filter(|d| d.id.starts_with(&id.to_lowercase())).map(|d| d.id.clone()).collect();
    match matches.as_slice() {
        [one] => {
            let peer = PeerId::from_hex(one).map_err(|e| e.to_string())?;
            agent.remove_device(&peer).map_err(|e| e.to_string())?;
            println!("Forgot {}.", short(&peer));
            Ok(())
        }
        [] => Err("No paired handheld has that id.".into()),
        _ => Err("More than one paired handheld starts with that; give more of the id.".into()),
    }
}

fn scan(agent: &Agent, json: bool) -> Result<(), String> {
    agent.rescan();
    let scan = agent.fresh_scan(u64::MAX);
    if json {
        println!("{}", serde_json::to_string_pretty(&scan).map_err(|e| e.to_string())?);
        return Ok(());
    }
    for f in &scan.games {
        let place = f.game.install.path.clone().unwrap_or_default();
        println!("{:<40} {:<28} {place}", f.game.title, f.game.key);
    }
    println!("{} games.", scan.games.len());
    for a in &scan.apps {
        println!("{:<40} {:<28} {}", a.name, a.source_label(), a.version.clone().unwrap_or_default());
    }
    println!("{} applications.", scan.apps.len());
    for p in &scan.programs {
        println!("syncs with: {} ({}) {}", p.name, p.note, p.path.display());
    }
    for p in &scan.problems {
        println!("could not read {p}");
    }
    Ok(())
}

fn folders(agent: &Agent, path: &str, roms: bool, add: bool) -> Result<(), String> {
    let path = PathBuf::from(path);
    if add && !path.is_dir() {
        return Err(format!("{} is not a folder.", path.display()));
    }
    {
        let mut s = agent.settings.lock().unwrap();
        if add {
            let list = if roms { &mut s.rom_folders } else { &mut s.game_folders };
            if !list.contains(&path) {
                list.push(path);
            }
        } else {
            s.game_folders.retain(|f| *f != path);
            s.rom_folders.retain(|f| *f != path);
        }
    }
    agent.save_settings().map_err(|e| e.to_string())
}

/// A game as the person typed it: a key (`steam:440`) or a title.
fn game_ref(text: &str) -> GameRef {
    if text.contains(':') && !text.contains(' ') {
        GameRef { key: text.to_string(), title: String::new() }
    } else {
        GameRef { key: title_key(text), title: text.to_string() }
    }
}

fn saves_add(agent: &Agent, game: &str, templates: &[&str]) -> Result<(), String> {
    for t in templates {
        if saves::split(t).is_none() {
            return Err(format!("{t} does not start with a known root: {}", saves::TOKENS.join(", ")));
        }
    }
    let key = game_ref(game).key;
    agent.saves.lock().unwrap().entry(key).or_default().patterns.extend(templates.iter().map(|t| t.to_string()));
    agent.save_saves().map_err(|e| e.to_string())
}

fn saves_show(agent: &Agent, game: &str) -> Result<(), String> {
    let game = game_ref(game);
    let Some((spec, roots, found)) = agent.save_lookup(&game, true) else {
        return Err("No save location is known for that game. State one with: droidtop-agent saves add <game> <template>".into());
    };
    if let Some(f) = found {
        println!("{} ({})", f.game.title, f.game.key);
    }
    for p in &spec.patterns {
        println!("  {p}");
    }
    for (name, path) in saves::collect(&spec, &roots) {
        println!("    {name}  ->  {}", path.display());
    }
    Ok(())
}
