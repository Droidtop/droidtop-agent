//! Lutris (Linux): its `pga.db`, and for a Wine game the prefix its own
//! settings name. A game Lutris installed from a store keeps
//! that store's id (`service`, `service_id`); others go by title. Lutris
//! measures play time, which is this computer's own and travels as such.

use std::path::PathBuf;

use droidtop_agent_core::library::title_key;

use super::{columns, home, DbCopy, Found};
use crate::state::Settings;

fn database() -> Option<PathBuf> {
    let h = home();
    [h.join(".local/share/lutris/pga.db"), h.join(".var/app/net.lutris.Lutris/data/lutris/pga.db")].into_iter().find(|p| p.is_file())
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let Some(path) = database() else { return Ok(Vec::new()) };
    let db = DbCopy::open(&path)?;
    let game_dirs = super::prefixes::lutris_game_dirs();
    let cols = columns(&db.conn, "games");
    let has = |c: &str| cols.iter().any(|x| x == c);
    if !has("name") || !has("installed") {
        return Err("its database changed shape".into());
    }
    let pick = |c: &str| if has(c) { c.to_string() } else { "NULL".to_string() };
    let sql = format!(
        "SELECT name, {}, {}, {}, {}, {}, {}, {} FROM games WHERE installed = 1",
        pick("directory"),
        pick("service"),
        pick("service_id"),
        pick("runner"),
        pick("playtime"),
        pick("lastplayed"),
        pick("configpath")
    );
    let mut stmt = db.conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .flatten()
        .map(|(name, dir, service, service_id, runner, hours, last, configpath)| {
            let key = match (service.as_deref(), service_id) {
                (Some(s @ ("steam" | "gog" | "epic" | "itch")), Some(id)) if !id.is_empty() => format!("{s}:{id}"),
                (Some("battlenet"), Some(id)) if !id.is_empty() => format!("battlenet:{id}"),
                _ => title_key(&name),
            };
            let base = dir.map(PathBuf::from).filter(|p| p.is_dir());
            let mut found = super::pc_game(key, name, base, "lutris", 0, None);
            // A Wine game's Windows saves are in the prefix its settings name.
            if runner.as_deref() == Some("wine") {
                found.prefix = configpath.as_deref().and_then(|c| super::prefixes::lutris(&game_dirs, c));
            }
            found.game.install.launcher = Some(format!("lutris/{}", runner.unwrap_or_default()));
            found.game.install.play_seconds = (hours.unwrap_or(0.0).max(0.0) * 3600.0) as u64;
            found.game.install.last_played_ms = last.unwrap_or(0).max(0) * 1000;
            found
        })
        .collect())
}
