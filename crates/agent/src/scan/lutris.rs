//! Lutris (Linux): its `pga.db`, and for a Wine game the prefix its own
//! settings name. A game Lutris installed from a store keeps
//! that store's id (`service`, `service_id`); others go by title. Lutris
//! measures play time, which is this computer's own and travels as such.
//! Its "favorite" and ".hidden" categories are the person's marks on those
//! games ([`marks`]); the agent reads them and never writes Lutris's database.

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

/// The library key Lutris's row for a game goes by: its store's id when Lutris
/// installed it from one, else its title.
fn key_of(name: &str, service: Option<&str>, service_id: Option<String>) -> String {
    match (service, service_id) {
        (Some(s @ ("steam" | "gog" | "epic" | "itch")), Some(id)) if !id.is_empty() => format!("{s}:{id}"),
        (Some("battlenet"), Some(id)) if !id.is_empty() => format!("battlenet:{id}"),
        _ => title_key(name),
    }
}

/// The person's marks on Lutris's installed games: `favourite` from its
/// "favorite" category and `hidden` from ".hidden", by library key. Every
/// installed game is listed, unset marks included, so a mark taken away in
/// Lutris reads as a change too.
pub fn marks() -> droidtop_agent_core::library::Marks {
    let Some(path) = database() else { return Default::default() };
    let Ok(db) = DbCopy::open(&path) else { return Default::default() };
    marks_in(&db.conn)
}

fn marks_in(conn: &rusqlite::Connection) -> droidtop_agent_core::library::Marks {
    use serde_json::Value;
    let mut out = droidtop_agent_core::library::Marks::new();
    let cols = columns(conn, "games");
    let has = |c: &str| cols.iter().any(|x| x == c);
    if !has("name") || !has("installed") || columns(conn, "games_categories").is_empty() {
        return out;
    }
    let pick = |c: &str| if has(c) { format!("g.{c}") } else { "NULL".to_string() };
    let sql = format!(
        "SELECT g.name, {}, {}, (SELECT group_concat(c.name, char(10)) FROM games_categories gc JOIN categories c ON c.id = gc.category_id WHERE gc.game_id = g.id) FROM games g WHERE g.installed = 1",
        pick("service"),
        pick("service_id"),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else { return out };
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
    });
    let Ok(rows) = rows else { return out };
    for (name, service, service_id, categories) in rows.flatten() {
        let categories: Vec<String> = categories.unwrap_or_default().lines().map(str::to_string).collect();
        let fields = out.entry(key_of(&name, service.as_deref(), service_id)).or_default();
        fields.insert("favourite".into(), Value::Bool(categories.iter().any(|c| c == "favorite")));
        fields.insert("hidden".into(), Value::Bool(categories.iter().any(|c| c == ".hidden")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favourites_and_hidden_come_from_lutris_categories() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE games (id INTEGER PRIMARY KEY, name TEXT, installed INTEGER, service TEXT, service_id TEXT);
             CREATE TABLE categories (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE games_categories (game_id INTEGER, category_id INTEGER);
             INSERT INTO games VALUES (1, 'Celeste', 1, 'gog', '1207658924'), (2, 'Hidden Thing', 1, NULL, NULL), (3, 'Gone', 0, NULL, NULL);
             INSERT INTO categories VALUES (1, 'favorite'), (2, '.hidden');
             INSERT INTO games_categories VALUES (1, 1), (2, 2), (3, 1);",
        )
        .unwrap();
        let marks = marks_in(&conn);
        assert_eq!(marks.len(), 2);
        assert_eq!(marks["gog:1207658924"]["favourite"], serde_json::json!(true));
        assert_eq!(marks["gog:1207658924"]["hidden"], serde_json::json!(false));
        assert_eq!(marks[&title_key("Hidden Thing")]["hidden"], serde_json::json!(true));
    }
}
