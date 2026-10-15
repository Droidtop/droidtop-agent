//! What the agent answers a paired handheld with: the core's [`Host`] over
//! the scan, the save catalog, the library and the context adapters.

use std::path::{Path, PathBuf};

use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::library::{title_key, Change};
use droidtop_agent_core::proto::{GameRef, Launch};
use droidtop_agent_core::saves::{prefix_roots, Roots, SaveSpec};
use droidtop_agent_core::server::Host;
use droidtop_agent_core::{Error, Result};
use droidtop_agent_scan::Found;

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
        if let Some(patterns) = crate::ludusavi::custom_games(&droidtop_agent_scan::apps::ludusavi_config()).remove(&title_key(&title)) {
            return Some((SaveSpec { patterns }, roots, found));
        }
        let folder = found.as_ref().and_then(|f| f.base.as_deref()).and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
        let spec = self.ludusavi_index(fetch)?.spec_for(&game.key, &title, folder.as_deref())?;
        Some((spec, roots, found))
    }
}

impl Agent {
    /// Puts a kept copy of a game's saves back where the game keeps them
    /// here. What is there now is kept first as this computer's own copy, so
    /// a restore can be undone.
    pub fn restore_saves(&self, game: &GameRef, copy: &Path) -> std::result::Result<usize, String> {
        let (spec, roots, _) = self.save_lookup(game, false).ok_or("No save location is known for this game; add one first.")?;
        let archive = self.dirs.archive().join(file_key(&game.key));
        // The copy being restored may itself be this computer's newest copy, which keeping would move.
        if !copy.starts_with(droidtop_agent_core::savesync::copy_dir(&archive, &self.name())) {
            droidtop_agent_core::savesync::keep_copy(
                droidtop_agent_core::saves::collect(&spec, &roots).into_iter(),
                &archive,
                &self.name(),
            )
            .map_err(|e| e.to_string())?;
        }
        if !copy.is_dir() {
            return Err("That copy is no longer kept: older copies are dropped as newer ones come.".into());
        }
        droidtop_agent_core::savesync::restore_copy(copy, &roots).map_err(|e| e.to_string())
    }
}

/// Where this computer's WireGuard answers from outside the LAN: the
/// endpoint the person forwarded, and every global IPv6 address it has
/// (no NAT in the way, only a firewall that lets the port in).
pub fn endpoints(public: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = public.into_iter().map(|p| format!("wg:{p}")).collect();
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if let std::net::IpAddr::V6(ip) = interface.ip() {
                if !ip.is_loopback() && ip.segments()[0] & 0xe000 == 0x2000 {
                    out.push(format!("wg:[{ip}]:{}", droidtop_agent_core::tunnel::WG_PORT));
                }
            }
        }
    }
    out.dedup();
    out
}

impl Host for Agent {
    fn name(&self) -> String {
        Agent::name(self)
    }

    fn primary(&self, peer: &PeerId) -> bool {
        self.is_primary(peer)
    }

    fn launch_game(&self, peer: &PeerId, game: &GameRef) -> Launch {
        // The settings are read again: the window or a command line may have
        // changed the switch since this process started.
        self.reload_settings();
        let installed = || self.fresh_scan(300).find(&game.key).is_some_and(|f| f.game.install.installed);
        let outcome = crate::launch::launch(self.may_launch(peer), installed, game, &crate::launch::Local);
        println!("{} asked to start {:?} ({:?}): {outcome:?}", self.device_name(peer), game.title, game.key);
        outcome
    }

    fn game_folder(&self, game: &GameRef) -> Option<(PathBuf, Option<String>)> {
        let scan = self.fresh_scan(300);
        let found = scan.find(&game.key).or_else(|| scan.find_title(&game.title))?;
        Some((found.base.clone().filter(|b| b.is_dir())?, found.game.install.version.clone()))
    }

    /// A version a handheld sends goes to the person's first game folder:
    /// never into a store's own install folders.
    fn game_inbox(&self) -> std::result::Result<PathBuf, String> {
        self.settings.lock().unwrap().game_folders.iter().find(|f| f.is_dir()).cloned().ok_or_else(|| {
            "this computer has no game folder to put it in: add one in droidtop-agent's Settings (or droidtop-agent folders add <path>)"
                .into()
        })
    }

    fn game_arrived(&self, game: &GameRef, folder: &Path) {
        println!("{} arrived in {}.", game.title, folder.display());
        self.rescan();
    }

    fn endpoints(&self) -> Vec<String> {
        let mut out = endpoints(self.settings.lock().unwrap().public_endpoint.as_deref());
        // The address STUN found for the WireGuard socket: enough on its own
        // when this side's NAT lets a known peer's packets in.
        if let Some(mapped) = self.rendezvous.lock().unwrap().mapped {
            out.push(format!("wg:{mapped}"));
        }
        out.dedup();
        out
    }

    fn disco_id(&self) -> Option<String> {
        if !self.settings.lock().unwrap().rendezvous {
            return None;
        }
        droidtop_agent_core::rendezvous::certificate(&self.key).ok().map(|c| c.device_id())
    }

    fn hello(&self, peer: &PeerId, disco: Option<&str>) {
        if let Some(disco) = disco {
            self.device_disco(peer, disco);
        }
    }

    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)> {
        self.save_lookup(game, true).map(|(spec, roots, _)| (spec, roots))
    }

    fn archive_dir(&self, game: &GameRef) -> PathBuf {
        let dir = self.dirs.archive().join(file_key(&game.key));
        // The folder is named for the key; the title is kept beside the copies so the
        // Saves page can name a game the library does not list (Droidtop/tracker#528).
        let title = dir.join("title.txt");
        if !game.title.is_empty() && std::fs::read_to_string(&title).ok().as_deref() != Some(game.title.as_str()) {
            let _ = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&title, &game.title));
        }
        dir
    }

    fn library_pull(&self, _peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)> {
        self.fresh_scan(120);
        Ok(self.library_now().changes_since(since))
    }

    fn library_push(&self, _peer: &PeerId, changes: Vec<Change>) -> Result<()> {
        self.edit_library(|lib| {
            let changed = changes.into_iter().filter(|c| lib.apply(c.clone())).count();
            ((), changed > 0)
        });
        Ok(())
    }

    fn context_pull(&self, context: &str, offer: Option<&AdapterOffer>) -> Result<Records> {
        if let Some(offer) = offer {
            self.consider_offer(context, offer).map_err(Error::Protocol)?;
        }
        self.adapter(context)
            .ok_or_else(|| no_adapter(context, offer.is_some_and(|o| o.for_this_system().is_some())))?
            .pull()
            .map_err(Error::Protocol)
    }

    fn context_push(&self, context: &str, changes: Vec<RecordChange>) -> Result<Option<String>> {
        self.adapter(context).ok_or_else(|| no_adapter(context, false))?.push(changes).map_err(Error::Protocol)
    }
}

/// Why a context cannot sync here; [`offered`] when the plugin offered its adapter.
fn no_adapter(context: &str, offered: bool) -> Error {
    Error::Protocol(if offered {
        format!("the computer waits for your OK to install the plugin's adapter for {context}: on the computer run droidtop-agent contexts approve {context}")
    } else {
        format!("this computer has no adapter for the {context} context; add the plugin's adapter with droidtop-agent contexts add")
    })
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
