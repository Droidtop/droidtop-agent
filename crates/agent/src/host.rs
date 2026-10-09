//! What the agent answers a paired handheld with: the core's [`Host`] over
//! the scan, the save catalog, the library and the context adapters.

use std::path::{Path, PathBuf};

use droidtop_agent_core::context::{RecordChange, Records};
use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::library::{title_key, Change};
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves::{prefix_roots, Roots, SaveSpec};
use droidtop_agent_core::server::Host;
use droidtop_agent_core::{Error, Result};

use crate::scan::Found;
use crate::state::Agent;

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

/// A game key as a folder name.
pub fn file_key(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

impl Agent {
    /// The Ludusavi index, loaded once; fetched when there is none yet and
    /// [`fetch`] allows it.
    pub fn ludusavi_index(&self, fetch: bool) -> Option<crate::ludusavi::Index> {
        let mut slot = self.ludusavi.lock().unwrap();
        if slot.is_none() {
            *slot = crate::ludusavi::load(&self.dirs.data);
        }
        if slot.is_none() && fetch {
            match crate::ludusavi::update(&self.dirs.data) {
                Ok(index) => *slot = Some(index),
                Err(e) => eprintln!("{e}"),
            }
        }
        slot.clone()
    }

    /// Where a game keeps its saves on this computer: the person's own entry
    /// first, then their Ludusavi custom games, then the Ludusavi manifest.
    pub fn save_lookup(&self, game: &GameRef, fetch: bool) -> Option<(SaveSpec, Roots, Option<Found>)> {
        let scan = self.fresh_scan(300);
        let found = scan.find(&game.key).or_else(|| scan.find_title(&game.title)).cloned();
        let user = {
            let saves = self.saves.lock().unwrap();
            saves.get(&game.key).or_else(|| saves.get(&title_key(&game.title))).cloned()
        };
        let roots = roots_for(found.as_ref(), user.as_ref().and_then(|u| u.base.clone()));
        if let Some(user) = user.filter(|u| !u.patterns.is_empty()) {
            return Some((SaveSpec { patterns: user.patterns }, roots, found));
        }
        let title = found.as_ref().map(|f| f.game.title.clone()).unwrap_or_else(|| game.title.clone());
        if let Some(patterns) = crate::ludusavi::custom_games(&crate::scan::apps::ludusavi_config()).remove(&title_key(&title)) {
            return Some((SaveSpec { patterns }, roots, found));
        }
        let folder = found.as_ref().and_then(|f| f.base.as_deref()).and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
        let spec = self.ludusavi_index(fetch)?.spec_for(&game.key, &title, folder.as_deref())?;
        Some((spec, roots, found))
    }
}

impl Host for Agent {
    fn name(&self) -> String {
        Agent::name(self)
    }

    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)> {
        self.save_lookup(game, true).map(|(spec, roots, _)| (spec, roots))
    }

    fn archive_dir(&self, game: &GameRef) -> PathBuf {
        self.dirs.archive().join(file_key(&game.key))
    }

    fn library_pull(&self, _peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)> {
        self.fresh_scan(120);
        Ok(self.library.lock().unwrap().changes_since(since))
    }

    fn library_push(&self, _peer: &PeerId, changes: Vec<Change>) -> Result<()> {
        let changed = {
            let mut lib = self.library.lock().unwrap();
            changes.into_iter().filter(|c| lib.apply(c.clone())).count()
        };
        if changed > 0 {
            self.save_library()?;
        }
        Ok(())
    }

    fn context_pull(&self, context: &str) -> Result<Records> {
        let adapter =
            crate::contexts::adapter(context).ok_or_else(|| Error::Protocol(format!("this computer has no {context} context")))?;
        adapter.pull().map_err(Error::Protocol)
    }

    fn context_push(&self, context: &str, changes: Vec<RecordChange>) -> Result<Option<String>> {
        let adapter =
            crate::contexts::adapter(context).ok_or_else(|| Error::Protocol(format!("this computer has no {context} context")))?;
        adapter.push(changes).map_err(Error::Protocol)
    }
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
            prefix: Some(crate::scan::Prefix { drive_c: PathBuf::from("/pfx/drive_c"), user: "steamuser".into() }),
        };
        let roots = roots_for(Some(&found), None);
        assert_eq!(roots["<winAppData>"], PathBuf::from("/pfx/drive_c/users/steamuser/AppData/Roaming"));
        assert_eq!(roots["<base>"], PathBuf::from("/games/G"));
    }
}
