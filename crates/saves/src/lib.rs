//! Where a game keeps its saves on this computer (docs/DESIGN.md section 6),
//! and the copies of them kept here (section 9): the person's own entries,
//! their Ludusavi custom games, the Ludusavi manifest's index, the folder
//! each save token means here (natively or inside a game's Wine or Proton
//! prefix), the kept-copies listing and restoring one.
//!
//! A crate of its own (section 13): the running agent answers handhelds
//! with it, the window and a desktop manager (Pythia) show it, and save
//! sources added later (emulators) build on it. It takes the scan it looks
//! games up in as an argument, so it needs no running agent.

use droidtop_agent_core::sync::LockExt;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use droidtop_agent_core::library::{title_key, Library};
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves::{prefix_roots, Roots, SaveSpec};
use droidtop_agent_core::savesync;
use droidtop_agent_scan::{Found, Scan};
use droidtop_agent_store::{read_json, write_json};
use serde::{Deserialize, Serialize};

pub mod copies;
pub mod engines;
pub mod ludusavi;

/// Save locations the person states for a game, ahead of the Ludusavi manifest.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UserSaves {
    #[serde(default)]
    pub patterns: Vec<String>,
    /// The game's folder, when the scanner does not know it.
    #[serde(default)]
    pub base: Option<PathBuf>,
}

/// The save catalog: the person's entries (`saves.json`) and the Ludusavi
/// index (kept in the agent's data folder).
pub struct Saves {
    file: PathBuf,
    data: PathBuf,
    /// The person's own entries, by game key or title key.
    pub user: Mutex<BTreeMap<String, UserSaves>>,
    ludusavi: Mutex<Option<ludusavi::Index>>,
}

impl Saves {
    /// The catalog over [`file`] (the person's entries) and [`data`] (where
    /// the Ludusavi index is kept).
    pub fn open(file: PathBuf, data: PathBuf) -> io::Result<Saves> {
        Ok(Saves { user: Mutex::new(read_json(&file)?), ludusavi: Mutex::new(None), file, data })
    }

    /// Writes the person's entries.
    pub fn save_user(&self) -> io::Result<()> {
        write_json(&self.file, &*self.user.locked())
    }

    /// The Ludusavi index, loaded once; fetched when there is none yet and
    /// [`fetch`] allows it. One fetch at a time: a second caller waits for
    /// the first one's index.
    pub fn ludusavi_index(&self, fetch: bool) -> Option<ludusavi::Index> {
        let mut slot = self.ludusavi.locked();
        if slot.is_none() {
            *slot = ludusavi::load(&self.data);
        }
        if slot.is_none() && fetch {
            match ludusavi::update(&self.data) {
                Ok(index) => *slot = Some(index),
                Err(e) => eprintln!("{e}"),
            }
        }
        slot.clone()
    }

    /// Where a game keeps its saves on this computer: the person's own entry
    /// first, then their Ludusavi custom games, then its engine's own rule
    /// ([`engines`]: Ren'Py's folders need no manifest), then the Ludusavi
    /// manifest (fetched first when there is none yet and [`fetch`] allows it).
    /// [`scan`] is where the game is looked up, by key and then by title.
    pub fn lookup(&self, game: &GameRef, scan: &Scan, fetch: bool) -> Option<(SaveSpec, Roots, Option<Found>)> {
        let found = scan.find(&game.key).or_else(|| scan.find_title(&game.title)).cloned();
        let user = {
            let saves = self.user.locked();
            saves.get(&game.key).or_else(|| saves.get(&title_key(&game.title))).cloned()
        };
        let roots = roots_for(found.as_ref(), user.as_ref().and_then(|u| u.base.clone()));
        if let Some(user) = user.filter(|u| !u.patterns.is_empty()) {
            return Some((SaveSpec { patterns: user.patterns }, roots, found));
        }
        let title = found.as_ref().map(|f| f.game.title.clone()).unwrap_or_else(|| game.title.clone());
        if let Some(patterns) = ludusavi::custom_games(&droidtop_agent_scan::apps::ludusavi_config()).remove(&title_key(&title)) {
            return Some((SaveSpec { patterns }, roots, found));
        }
        if let Some(spec) = found.as_ref().and_then(|f| engines::spec_for(f.engine.as_deref()?, f.base.as_deref()?, &title, &roots)) {
            return Some((spec, roots, found));
        }
        let folder = found.as_ref().and_then(|f| f.base.as_deref()).and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
        let spec = self.ludusavi_index(fetch)?.spec_for(&game.key, &title, folder.as_deref())?;
        Some((spec, roots, found))
    }

    /// Puts a kept copy of a game's saves back where the game keeps them
    /// here. What is there now is kept first as [`device`]'s own copy in
    /// [`archive`] (the agent's archive folder), so a restore can be undone.
    pub fn restore(&self, game: &GameRef, copy: &Path, scan: &Scan, archive: &Path, device: &str) -> Result<usize, String> {
        let (spec, roots, _) = self.lookup(game, scan, false).ok_or("No save location is known for this game; add one first.")?;
        let archive = archive.join(file_key(&game.key));
        // The copy being restored may itself be this computer's newest copy, which keeping would move.
        if !copy.starts_with(savesync::copy_dir(&archive, device)) {
            savesync::keep_copy(droidtop_agent_core::saves::collect(&spec, &roots).into_iter(), &archive, device)
                .map_err(|e| e.to_string())?;
        }
        if !copy.is_dir() {
            return Err("That copy is no longer kept: older copies are dropped as newer ones come.".into());
        }
        savesync::restore_copy(copy, &roots).map_err(|e| e.to_string())
    }
}

/// This computer's folder for each token, for a game found by the scan.
pub fn roots_for(found: Option<&Found>, base: Option<PathBuf>) -> Roots {
    let mut roots = match found.and_then(|f| f.prefix.as_ref()) {
        // A game that runs under Proton or Wine here keeps its Windows saves in the prefix.
        Some(prefix) => prefix_roots(&prefix.drive_c, &prefix.user),
        None => native_roots(),
    };
    if let Some(base) = base.or_else(|| found.and_then(|f| f.base.clone())) {
        roots.insert("<base>".into(), base);
    }
    roots
}

fn native_roots() -> Roots {
    let mut r = Roots::new();
    let home = dirs::home_dir().unwrap_or_default();
    r.insert("<home>".into(), home.clone());
    if cfg!(windows) {
        let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
        if let Some(d) = dirs::config_dir() {
            r.insert("<winAppData>".into(), d);
        }
        if let Some(d) = dirs::data_local_dir() {
            r.insert("<winLocalAppData>".into(), d);
        }
        r.insert("<winLocalAppDataLow>".into(), home.join("AppData/LocalLow"));
        r.insert("<winDocuments>".into(), dirs::document_dir().unwrap_or_else(|| home.join("Documents")));
        for (token, var) in [("<winPublic>", "PUBLIC"), ("<winProgramData>", "ProgramData"), ("<winDir>", "WINDIR")] {
            if let Some(d) = env(var) {
                r.insert(token.into(), d);
            }
        }
    } else {
        if let Some(d) = dirs::data_dir() {
            r.insert("<xdgData>".into(), d);
        }
        if let Some(d) = dirs::config_dir() {
            r.insert("<xdgConfig>".into(), d);
        }
    }
    r
}

/// A game key as a folder name: each game's folder in the archive.
pub fn file_key(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Each archive folder's game, from the library: [`file_key`] of a game's
/// key, to its key and title (what [`copies::kept`] names the folders by).
pub fn library_titles(library: &Library) -> BTreeMap<String, (String, String)> {
    library.games.iter().map(|(k, g)| (file_key(k), (k.clone(), g.title.clone()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_keys_are_safe_names() {
        assert_eq!(file_key("steam:440"), "steam_440");
        assert_eq!(file_key("title:a/b"), "title_a_b");
    }

    #[test]
    fn a_proton_game_resolves_inside_its_prefix() {
        let found = Found {
            game: droidtop_agent_core::library::ScannedGame {
                key: "steam:1".into(),
                title: "G".into(),
                platform: None,
                install: Default::default(),
            },
            base: Some(PathBuf::from("/games/G")),
            prefix: Some(droidtop_agent_scan::Prefix { drive_c: PathBuf::from("/pfx/drive_c"), user: "steamuser".into() }),
            engine: None,
        };
        let roots = roots_for(Some(&found), None);
        assert_eq!(roots["<winAppData>"], PathBuf::from("/pfx/drive_c/users/steamuser/AppData/Roaming"));
        assert_eq!(roots["<base>"], PathBuf::from("/games/G"));
    }
}
