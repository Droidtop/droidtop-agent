//! droidtop-agent: keeps this computer and droidtop on a handheld in step
//! (docs/DESIGN.md). Run without arguments it serves paired handhelds.

use std::path::PathBuf;
use std::sync::Arc;

use droidtop_agent_core::keys::{short, PeerId};
use droidtop_agent_core::library::title_key;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves;

use droidtop_agent_saves::ludusavi;
use droidtop_agent_service::{autostart, host, rendezvous, serve, Agent};
use droidtop_agent_store::read_json;
use {droidtop_agent_contexts as contexts, droidtop_agent_pair as pair};

const HELP: &str = "droidtop-agent: keeps this computer and droidtop on a handheld in step.

Usage:
  droidtop-agent [run]                 Serve paired handhelds (keep it running)
  droidtop-agent pair <code>           Pair with a handheld showing a pairing code
                                       (droidtop: Settings > Computers > Pair a computer).
                                       <code> is the 6 digits, or the text of its QR code.
  droidtop-agent pair                  Show this computer's address and a code to type on the handheld
                                       (droidtop: Pair a computer > Use a code from the computer),
                                       for a handheld this computer cannot reach
  droidtop-agent pair compare          Offer to pair: a handheld or another computer picks this one
                                       from its list, and both screens show one code to confirm
  droidtop-agent pair find             Pick a computer offering to pair, and confirm the code
  droidtop-agent devices               List paired handhelds
  droidtop-agent unpair <id>           Forget a handheld (the start of its id is enough)
  droidtop-agent id                    Show this computer's name and id
  droidtop-agent name <name>           Set the name the handheld shows for this computer
  droidtop-agent scan [--json]         Show the games and apps found on this computer
  droidtop-agent folders               List game and ROM folders
  droidtop-agent folders add [--roms] <path>
  droidtop-agent folders remove <path>
  droidtop-agent mark <game> favourite|hidden|completed on|off
  droidtop-agent mark <game> collections <name>[,<name>...]
  droidtop-agent marks <game>          Your marks on a game, from this computer (they reach your handhelds
                                       when they sync, like marks made on a handheld); <game> is a key such
                                       as steam:440 or a title in quotes
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
  droidtop-agent primary [<id> | none] The handheld whose saves win here when both changed (the start of its id
                                       is enough); with none, the newest copy wins
  droidtop-agent launch                Which handhelds may start games on this computer (none until you allow one)
  droidtop-agent launch allow <id>     Let a handheld start Steam games here, e.g. for streaming them with
                                       Steam Link (the start of its id is enough)
  droidtop-agent launch deny <id>
  droidtop-agent autostart [on | off]  Start when you sign in (a systemd user unit or autostart entry on
                                       Linux, a LaunchAgent on macOS, the Run key on Windows); off unless
                                       you turn it on
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
        ["pair", "compare"] => pair::compare_offer(&agent.store).map(|name| println!("Paired with {name}.")),
        ["pair", "find"] => pair::compare_find(&agent.store).map(|name| println!("Paired with {name}.")),
        ["pair", rest @ ..] if !rest.is_empty() => pair::pair(&agent.store, &rest.join(" ")).map(|name| println!("Paired with {name}.")),
        ["pair"] => pair::show(&agent.store).map(|name| println!("Paired with {name}.")),
        ["devices"] => {
            devices(&agent);
            Ok(())
        }
        ["unpair", id] => unpair(&agent, id),
        ["id"] => {
            println!("{}\n{}", agent.store.name(), agent.store.peer_id());
            Ok(())
        }
        ["name", rest @ ..] if !rest.is_empty() => {
            agent.store.settings.lock().unwrap().name = Some(rest.join(" "));
            agent.store.save_settings().map_err(|e| e.to_string())
        }
        ["scan"] => scan(&agent, false),
        ["scan", "--json"] => scan(&agent, true),
        ["folders"] => {
            let s = agent.store.settings.lock().unwrap();
            s.game_folders.iter().for_each(|f| println!("games  {}", f.display()));
            s.rom_folders.iter().for_each(|f| println!("roms   {}", f.display()));
            Ok(())
        }
        ["folders", "add", "--roms", path] => folders(&agent, path, true, true),
        ["folders", "add", path] => folders(&agent, path, false, true),
        ["folders", "remove", path] => folders(&agent, path, false, false),
        ["mark", game, field @ ("favourite" | "hidden" | "completed"), on @ ("on" | "off")] => {
            let key = game_ref(game).key;
            agent.store.set_mark(&key, field, serde_json::Value::Bool(*on == "on"));
            println!("{key}: {field} {on}");
            Ok(())
        }
        ["mark", game, "collections", names @ ..] if !names.is_empty() => {
            let mut list: Vec<String> = names.join(" ").split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
            list.sort();
            list.dedup();
            let key = game_ref(game).key;
            agent.store.set_mark(&key, "collections", serde_json::json!(list));
            println!("{key}: collections {}", list.join(", "));
            Ok(())
        }
        ["marks", game] => {
            let key = game_ref(game).key;
            match agent.store.library_now().games.get(&key) {
                Some(record) if !record.marks.is_empty() => {
                    for (field, m) in &record.marks {
                        println!("{field}: {}", m.value);
                    }
                }
                _ => println!("No marks are set on that game."),
            }
            Ok(())
        }
        ["saves", "add", game, templates @ ..] if !templates.is_empty() => saves_add(&agent, game, templates),
        ["saves", "remove", game] => {
            let removed = agent.saves.user.lock().unwrap().remove(&game_ref(game).key).is_some();
            agent
                .saves
                .save_user()
                .map_err(|e| e.to_string())
                .map(|()| println!("{}", if removed { "Removed." } else { "There was nothing set for that game." }))
        }
        ["saves", rest @ ..] if !rest.is_empty() => saves_show(&agent, &rest.join(" ")),
        ["manifest", "update"] => ludusavi::update(&agent.store.dirs.data)
            .map(|i| println!("The Ludusavi manifest has save locations for {} games.", i.games.len())),
        ["share"] => {
            match &agent.store.settings.lock().unwrap().share {
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
                agent.store.settings.lock().unwrap().share = Some(path);
                agent.store.save_settings().map_err(|e| e.to_string())
            }
        }
        ["share", "clear"] => {
            agent.store.settings.lock().unwrap().share = None;
            agent.store.save_settings().map_err(|e| e.to_string())
        }
        ["contexts"] => {
            let adapters = agent.store.settings.lock().unwrap().adapters.clone();
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
        ["contexts", "add", rest @ ..] if !rest.is_empty() => {
            let given = rest.join(" ");
            std::fs::canonicalize(&given).map_err(|e| format!("{given}: {e}")).and_then(|program| {
                let (id, what) = contexts::describe(&program)?;
                agent.store.settings.lock().unwrap().adapters.insert(id.clone(), program);
                agent.store.save_settings().map_err(|e| e.to_string())?;
                println!("Added the {id} context: {what}");
                Ok(())
            })
        }
        ["contexts", "approve", id] => contexts::approve(&agent.store, id).map(|line| println!("{line}")),
        ["contexts", "remove", id] => {
            let removed = agent.store.settings.lock().unwrap().adapters.remove(*id).is_some();
            agent
                .store
                .save_settings()
                .map_err(|e| e.to_string())
                .map(|()| println!("{}", if removed { "Removed." } else { "No adapter serves that context." }))
        }
        ["endpoint"] => {
            for e in host::endpoints(agent.store.settings.lock().unwrap().public_endpoint.as_deref()) {
                println!("{e}");
            }
            Ok(())
        }
        ["endpoint", "set", at] => match at.parse::<std::net::SocketAddr>() {
            Ok(_) => {
                agent.store.settings.lock().unwrap().public_endpoint = Some(at.to_string());
                agent.store.save_settings().map_err(|e| e.to_string())
            }
            Err(_) => Err(format!("{at} is not an address and port, such as 203.0.113.7:47611.")),
        },
        ["rendezvous"] => {
            let s = agent.store.settings.lock().unwrap().clone();
            println!("Rendezvous: {}", if s.rendezvous { "on" } else { "off" });
            println!("Discovery servers: {}", s.discovery_servers.join(" "));
            println!("STUN servers: {}", s.stun_servers.join(" "));
            if let Ok(cert) = droidtop_agent_core::rendezvous::certificate(&agent.store.key) {
                println!("This computer's discovery ID: {}", cert.device_id());
            }
            match read_json::<rendezvous::Status>(&agent.store.dirs.rendezvous()) {
                Ok(st) if st.disco.is_some() => println!("{st:#?}"),
                _ => println!("No rendezvous state yet: it is kept while droidtop-agent runs."),
            }
            Ok(())
        }
        ["rendezvous", on @ ("on" | "off")] => {
            agent.store.settings.lock().unwrap().rendezvous = *on == "on";
            agent.store.save_settings().map_err(|e| e.to_string())
        }
        ["rendezvous", "servers", list @ ..] if !list.is_empty() => {
            match list.iter().find(|s| **s != "default" && droidtop_agent_core::rendezvous::Server::parse(s).is_none()) {
                Some(bad) => Err(format!("{bad} is not an https discovery server address (or default).")),
                None => {
                    agent.store.settings.lock().unwrap().discovery_servers = list.iter().map(|s| s.to_string()).collect();
                    agent.store.save_settings().map_err(|e| e.to_string())
                }
            }
        }
        ["rendezvous", "stun", list @ ..] if !list.is_empty() => {
            agent.store.settings.lock().unwrap().stun_servers = list.iter().map(|s| s.to_string()).collect();
            agent.store.save_settings().map_err(|e| e.to_string())
        }
        ["primary"] => {
            let id = agent.store.settings.lock().unwrap().primary_device.clone();
            match id.and_then(|id| PeerId::from_hex(&id).ok()) {
                Some(peer) => println!("Primary handheld: {}", agent.store.device_name(&peer)),
                None => println!("No primary handheld: the newest copy of a game's saves wins."),
            }
            Ok(())
        }
        ["primary", "none"] => {
            agent.store.settings.lock().unwrap().primary_device = None;
            agent.store.save_settings().map_err(|e| e.to_string())
        }
        ["primary", id] => {
            let matches: Vec<String> =
                agent.store.devices_now().iter().filter(|d| d.id.starts_with(&id.to_lowercase())).map(|d| d.id.clone()).collect();
            match matches.as_slice() {
                [one] => {
                    agent.store.settings.lock().unwrap().primary_device = Some(one.clone());
                    agent.store.save_settings().map_err(|e| e.to_string())
                }
                [] => Err("No paired handheld has that id.".into()),
                _ => Err("More than one paired handheld starts with that; give more of the id.".into()),
            }
        }
        ["launch"] => {
            let allowed = agent.store.settings.lock().unwrap().launch_devices.clone();
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
        ["launch", verb @ ("allow" | "deny"), id] => launch_switch(&agent, id, *verb == "allow"),
        ["autostart"] => {
            println!("Starting when you sign in: {}", if autostart::enabled() { "on" } else { "off" });
            Ok(())
        }
        ["autostart", on @ ("on" | "off")] => autostart::set(*on == "on").map(|said| println!("{said}")),
        ["endpoint", "clear"] => {
            agent.store.settings.lock().unwrap().public_endpoint = None;
            agent.store.save_settings().map_err(|e| e.to_string())
        }
        _ => Err(format!("Unknown command.\n\n{HELP}")),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn devices(agent: &Agent) {
    let devices = agent.store.devices_now();
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
        agent.store.devices_now().iter().filter(|d| d.id.starts_with(&id.to_lowercase())).map(|d| d.id.clone()).collect();
    match matches.as_slice() {
        [one] => {
            let peer = PeerId::from_hex(one).map_err(|e| e.to_string())?;
            agent.store.remove_device(&peer).map_err(|e| e.to_string())?;
            println!("Forgot {}.", short(&peer));
            Ok(())
        }
        [] => Err("No paired handheld has that id.".into()),
        _ => Err("More than one paired handheld starts with that; give more of the id.".into()),
    }
}

fn launch_switch(agent: &Agent, id: &str, allow: bool) -> Result<(), String> {
    let matches: Vec<_> = agent.store.devices_now().into_iter().filter(|d| d.id.starts_with(&id.to_lowercase())).collect();
    match matches.as_slice() {
        [one] => {
            {
                let mut s = agent.store.settings.lock().unwrap();
                s.launch_devices.retain(|d| *d != one.id);
                if allow {
                    s.launch_devices.push(one.id.clone());
                }
            }
            agent.store.save_settings().map_err(|e| e.to_string())?;
            println!("{} {} start games on this computer.", one.name, if allow { "may now" } else { "may no longer" });
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
        let mut s = agent.store.settings.lock().unwrap();
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
    agent.store.save_settings().map_err(|e| e.to_string())
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
    agent.saves.user.lock().unwrap().entry(key).or_default().patterns.extend(templates.iter().map(|t| t.to_string()));
    agent.saves.save_user().map_err(|e| e.to_string())
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
