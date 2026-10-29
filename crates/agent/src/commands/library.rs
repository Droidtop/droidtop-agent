//! `scan`, `folders`, `mark`, `marks`: this computer's games and what the person says about them.

use std::path::PathBuf;
use std::sync::Arc;

use droidtop_agent_core::sync::LockExt;
use droidtop_agent_service::Agent;

use super::{change, game_ref, usage};

pub fn scan(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let json = match args {
        [] => false,
        ["--json"] => true,
        _ => return Err(usage("scan")),
    };
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

pub fn folders(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [] => {
            let s = agent.store.settings.locked();
            s.game_folders.iter().for_each(|f| println!("games  {}", f.display()));
            s.rom_folders.iter().for_each(|f| println!("roms   {}", f.display()));
            Ok(())
        }
        ["add", "--roms", path] => add_folder(agent, path, true),
        ["add", path] => add_folder(agent, path, false),
        ["remove", path] => {
            let path = PathBuf::from(path);
            change(agent, |s| {
                s.game_folders.retain(|f| *f != path);
                s.rom_folders.retain(|f| *f != path);
            })
        }
        _ => Err(usage("folders")),
    }
}

fn add_folder(agent: &Agent, path: &str, roms: bool) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.is_dir() {
        return Err(format!("{} is not a folder.", path.display()));
    }
    change(agent, |s| {
        let list = if roms { &mut s.rom_folders } else { &mut s.game_folders };
        if !list.contains(&path) {
            list.push(path);
        }
    })
}

pub fn mark(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        [game, field @ ("favourite" | "hidden" | "completed"), on @ ("on" | "off")] => {
            let key = game_ref(game).key;
            agent.store.set_mark(&key, field, serde_json::Value::Bool(*on == "on"));
            println!("{key}: {field} {on}");
            Ok(())
        }
        [game, "collections", names @ ..] if !names.is_empty() => {
            let mut list: Vec<String> = names.join(" ").split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
            list.sort();
            list.dedup();
            let key = game_ref(game).key;
            agent.store.set_mark(&key, "collections", serde_json::json!(list));
            println!("{key}: collections {}", list.join(", "));
            Ok(())
        }
        _ => Err(usage("mark")),
    }
}

pub fn marks(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let [game] = args else { return Err(usage("marks")) };
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
