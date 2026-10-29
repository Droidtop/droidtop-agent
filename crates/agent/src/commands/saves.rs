//! `saves`, `manifest`: where games keep their saves on this computer.

use std::sync::Arc;

use droidtop_agent_core::saves;
use droidtop_agent_core::sync::LockExt;
use droidtop_agent_saves::ludusavi;
use droidtop_agent_service::Agent;

use super::{game_ref, usage};

pub fn saves(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        ["add", game, templates @ ..] if !templates.is_empty() => add(agent, game, templates),
        ["remove", game] => {
            let removed = agent.saves.user.locked().remove(&game_ref(game).key).is_some();
            agent
                .saves
                .save_user()
                .map_err(|e| e.to_string())
                .map(|()| println!("{}", if removed { "Removed." } else { "There was nothing set for that game." }))
        }
        [] => Err(usage("saves")),
        game => show(agent, &game.join(" ")),
    }
}

fn add(agent: &Agent, game: &str, templates: &[&str]) -> Result<(), String> {
    for t in templates {
        if saves::split(t).is_none() {
            return Err(format!("{t} does not start with a known root: {}", saves::TOKENS.join(", ")));
        }
    }
    let key = game_ref(game).key;
    agent.saves.user.locked().entry(key).or_default().patterns.extend(templates.iter().map(|t| t.to_string()));
    agent.saves.save_user().map_err(|e| e.to_string())
}

fn show(agent: &Agent, game: &str) -> Result<(), String> {
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

pub fn manifest(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    match args {
        ["update"] => ludusavi::update(&agent.store.dirs.data)
            .map(|i| println!("The Ludusavi manifest has save locations for {} games.", i.games.len())),
        _ => Err(usage("manifest")),
    }
}
