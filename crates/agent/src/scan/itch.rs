//! itch: the itch app's own database, `db/butler.db` (its caves are the
//! installs, joined with games for titles and install locations for paths).

use std::path::PathBuf;

use super::{env_dir, home, pc_game, DbCopy, Found};
use crate::state::Settings;

fn database() -> Option<PathBuf> {
    let h = home();
    let mut candidates = vec![h.join(".config/itch/db/butler.db"), h.join("Library/Application Support/itch/db/butler.db")];
    if let Some(appdata) = env_dir("APPDATA") {
        candidates.insert(0, appdata.join(r"itch\db\butler.db"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    let Some(path) = database() else { return Ok(Vec::new()) };
    let db = DbCopy::open(&path)?;
    let mut stmt = db
        .conn
        .prepare(
            "SELECT c.game_id, g.title, l.path, c.install_folder_name, c.installed_size \
             FROM caves c JOIN games g ON g.id = c.game_id \
             LEFT JOIN install_locations l ON l.id = c.install_location_id",
        )
        .map_err(|e| format!("its database changed shape ({e})"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .flatten()
        .map(|(id, title, location, folder, size)| {
            let base = match (location, folder) {
                (Some(l), Some(f)) => Some(PathBuf::from(l).join(f)).filter(|p| p.is_dir()),
                _ => None,
            };
            pc_game(
                format!("itch:{id}"),
                title.unwrap_or_else(|| format!("itch game {id}")),
                base,
                "itch",
                size.unwrap_or(0).max(0) as u64,
                None,
            )
        })
        .collect())
}
