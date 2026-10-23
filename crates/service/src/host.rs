//! What the agent answers a paired handheld with: the core's [`Host`] over
//! the scan, the save catalog, the library and the context adapters.

use std::path::{Path, PathBuf};

use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::library::Change;
use droidtop_agent_core::proto::{Announcements, GameRef, Launch, Offer};
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_core::server::Host;
use droidtop_agent_core::{Error, Result};
use droidtop_agent_saves::file_key;
use serde_json::json;

use crate::Agent;

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
        self.store.name()
    }

    fn extra_features(&self) -> Vec<String> {
        if self.launcher.lock().unwrap().is_some() {
            vec!["launch_host".into()]
        } else {
            Vec::new()
        }
    }

    fn primary(&self, peer: &PeerId) -> bool {
        self.store.is_primary(peer)
    }

    fn offers(&self, peer: &PeerId) -> Offer {
        // Read again, as for starting a game: the window or a command line
        // may have changed it since this process started.
        self.store.reload_settings();
        self.store.offers_to(peer)
    }

    fn syncs_contexts(&self, peer: &PeerId) -> bool {
        self.store.syncs_contexts(peer)
    }

    fn launch_game(&self, peer: &PeerId, game: &GameRef) -> Launch {
        // The settings are read again: the window or a command line may have
        // changed the switch since this process started.
        self.store.reload_settings();
        let installed = || self.fresh_scan(300).find(&game.key).is_some_and(|f| f.game.install.installed);
        let host = self.launcher.lock().unwrap().clone();
        let outcome = crate::launch::launch(self.store.may_launch(peer), installed, game, &crate::launch::Local, host.as_deref());
        let device = self.store.device_name(peer);
        self.events.info(
            "launch",
            format!("{device} asked to start {:?} ({:?}): {outcome:?}", game.title, game.key),
            json!({ "peer": peer.to_hex(), "device": device, "title": game.title, "key": game.key, "outcome": outcome }),
        );
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
        self.store.settings.lock().unwrap().game_folders.iter().find(|f| f.is_dir()).cloned().ok_or_else(|| {
            "this computer has no game folder to put it in: add one in droidtop-agent's Settings (or droidtop-agent folders add <path>)"
                .into()
        })
    }

    fn game_arrived(&self, game: &GameRef, folder: &Path) {
        self.events.info(
            "arrived",
            format!("{} arrived in {}.", game.title, folder.display()),
            json!({ "title": game.title, "key": game.key, "folder": folder }),
        );
        self.rescan();
    }

    fn saves_synced(&self, peer: &PeerId, game: &GameRef, sent: usize, received: usize, removed: usize) {
        let device = self.store.device_name(peer);
        let mut done = Vec::new();
        if received > 0 {
            done.push(format!("took {received} save files from {device}"));
        }
        if removed > 0 {
            done.push(format!("removed {removed} that {device} no longer has"));
        }
        if sent > 0 {
            done.push(format!("sent {sent} to {device}"));
        }
        self.events.info(
            "saves",
            format!("Saves of {}: {}.", game.title, done.join(", ")),
            json!({ "peer": peer.to_hex(), "device": device, "title": game.title, "key": game.key, "sent": sent, "received": received, "removed": removed }),
        );
    }

    fn announce(&self, peer: &PeerId, theirs: Announcements) -> Result<Announcements> {
        if self.store.device_announced(peer, theirs.clone()) {
            let device = self.store.device_name(peer);
            let plugins: Vec<&String> = theirs.keys().collect();
            self.events.info(
                "announce",
                format!("{device} announced for {}.", if plugins.is_empty() { "no plugin".to_string() } else { format!("{plugins:?}") }),
                json!({ "peer": peer.to_hex(), "device": device, "announced": theirs }),
            );
        }
        self.store.reload_settings();
        Ok(self.store.settings.lock().unwrap().announce.clone())
    }

    fn endpoints(&self) -> Vec<String> {
        let mut out = endpoints(self.store.settings.lock().unwrap().public_endpoint.as_deref());
        // The address STUN found for the WireGuard socket: enough on its own
        // when this side's NAT lets a known peer's packets in.
        if let Some(mapped) = self.rendezvous.lock().unwrap().mapped {
            out.push(format!("wg:{mapped}"));
        }
        out.dedup();
        out
    }

    fn disco_id(&self) -> Option<String> {
        if !self.store.settings.lock().unwrap().rendezvous {
            return None;
        }
        droidtop_agent_core::rendezvous::certificate(&self.store.key).ok().map(|c| c.device_id())
    }

    fn hello(&self, peer: &PeerId, disco: Option<&str>) {
        if let Some(disco) = disco {
            self.store.device_disco(peer, disco);
        }
    }

    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)> {
        self.save_lookup(game, true).map(|(spec, roots, _)| (spec, roots))
    }

    fn archive_dir(&self, game: &GameRef) -> PathBuf {
        let dir = self.store.dirs.archive().join(file_key(&game.key));
        // The folder is named for the key; the title is kept beside the copies so the
        // Saves page can name a game the library does not list (Droidtop/tracker#528).
        let title = dir.join("title.txt");
        if !game.title.is_empty() && std::fs::read_to_string(&title).ok().as_deref() != Some(game.title.as_str()) {
            let _ = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&title, &game.title));
        }
        dir
    }

    fn library_pull(&self, peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)> {
        self.fresh_scan(120);
        let library = self.store.library_now();
        let (mut changes, cursor) = library.changes_since(since);
        // A computer is offered this computer's own games, never what it
        // knows of other devices' (docs/DESIGN.md section 20): their installs
        // and the marks on games only they have stay here, and so do the
        // applications the scan lists (`app:` keys), which are no games.
        if self.store.is_computer(peer) {
            let own = self.store.peer_id().to_hex();
            let mine = |game: &str| !game.starts_with("app:") && library.games.get(game).is_some_and(|g| g.installs.contains_key(&own));
            changes.retain(|c| match c {
                Change::Install { game, device, .. } => *device == own && mine(game),
                Change::Mark { game, .. } => mine(game),
            });
        }
        if !changes.is_empty() {
            let device = self.store.device_name(peer);
            self.events.info(
                "library",
                format!("Library: sent {} changes to {device}.", changes.len()),
                json!({ "peer": peer.to_hex(), "device": device, "sent": changes.len() }),
            );
        }
        Ok((changes, cursor))
    }

    fn library_push(&self, peer: &PeerId, changes: Vec<Change>) -> Result<()> {
        let changed = self.store.edit_library(|lib| {
            let changed = changes.into_iter().filter(|c| lib.apply(c.clone())).count();
            (changed, changed > 0)
        });
        if changed > 0 {
            let device = self.store.device_name(peer);
            self.events.info(
                "library",
                format!("Library: {changed} changes from {device}."),
                json!({ "peer": peer.to_hex(), "device": device, "received": changed }),
            );
        }
        Ok(())
    }

    fn context_pull(&self, context: &str, offer: Option<&AdapterOffer>) -> Result<Records> {
        if let Some(offer) = offer {
            droidtop_agent_contexts::consider_offer(&self.store, context, offer).map_err(Error::Protocol)?;
        }
        droidtop_agent_contexts::adapter(&self.store, context)
            .ok_or_else(|| no_adapter(context, offer.is_some_and(|o| o.for_this_system().is_some())))?
            .pull()
            .map_err(Error::Protocol)
    }

    fn context_push(&self, context: &str, changes: Vec<RecordChange>) -> Result<Option<String>> {
        droidtop_agent_contexts::adapter(&self.store, context)
            .ok_or_else(|| no_adapter(context, false))?
            .push(changes)
            .map_err(Error::Protocol)
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
    use droidtop_agent_core::keys::DeviceKey;
    use droidtop_agent_core::pairing::Kind;
    use droidtop_agent_store::Dirs;

    #[test]
    fn a_handhelds_announcement_is_kept_and_answered_with_this_computers() {
        let root = std::env::temp_dir().join(format!("dta-announce-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs { config: root.join("config"), data: root.join("data"), computer: root.join("host") };
        for d in [&dirs.config, &dirs.data, &dirs.computer] {
            std::fs::create_dir_all(d).unwrap();
        }
        let agent = Agent::open_in(dirs).unwrap();
        let handheld = DeviceKey::generate().peer_id();
        agent.store.add_device(handheld, "Pocket", Kind::Handheld).unwrap();
        agent.store.set_announce("syncthing", Some(json!({ "syncthing": "PC-ID" }))).unwrap();
        assert!(agent.store.set_announce("not a plugin", Some(json!(1))).is_err());

        let theirs: Announcements = [("syncthing".to_string(), json!({ "syncthing": "HH-ID" }))].into();
        let mine = Host::announce(&agent, &handheld, theirs.clone()).unwrap();
        assert_eq!(mine, [("syncthing".to_string(), json!({ "syncthing": "PC-ID" }))].into());
        let device = agent.store.devices_now().into_iter().find(|d| d.id == handheld.to_hex()).unwrap();
        assert_eq!(device.announced, theirs);
        assert_eq!(device.kind, Some(Kind::Handheld));
        let said: Vec<&str> = agent.events.since(0).iter().map(|e| e.kind).collect();
        assert_eq!(said, ["announce"]);
        // The same again changes nothing and says nothing.
        Host::announce(&agent, &handheld, theirs).unwrap();
        assert_eq!(agent.events.since(0).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
