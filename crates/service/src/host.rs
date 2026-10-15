//! What the agent answers a paired handheld with: the core's [`Host`] over
//! the scan, the save catalog, the library and the context adapters.

use std::path::{Path, PathBuf};

use droidtop_agent_core::context::{AdapterOffer, RecordChange, Records};
use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::library::Change;
use droidtop_agent_core::proto::{GameRef, Launch};
use droidtop_agent_core::saves::{Roots, SaveSpec};
use droidtop_agent_core::server::Host;
use droidtop_agent_core::{Error, Result};
use droidtop_agent_saves::file_key;

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

    fn primary(&self, peer: &PeerId) -> bool {
        self.store.is_primary(peer)
    }

    fn launch_game(&self, peer: &PeerId, game: &GameRef) -> Launch {
        // The settings are read again: the window or a command line may have
        // changed the switch since this process started.
        self.store.reload_settings();
        let installed = || self.fresh_scan(300).find(&game.key).is_some_and(|f| f.game.install.installed);
        let outcome = crate::launch::launch(self.store.may_launch(peer), installed, game, &crate::launch::Local);
        println!("{} asked to start {:?} ({:?}): {outcome:?}", self.store.device_name(peer), game.title, game.key);
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
        println!("{} arrived in {}.", game.title, folder.display());
        self.rescan();
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

    fn library_pull(&self, _peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)> {
        self.fresh_scan(120);
        Ok(self.store.library_now().changes_since(since))
    }

    fn library_push(&self, _peer: &PeerId, changes: Vec<Change>) -> Result<()> {
        self.store.edit_library(|lib| {
            let changed = changes.into_iter().filter(|c| lib.apply(c.clone())).count();
            ((), changed > 0)
        });
        Ok(())
    }

    fn context_pull(&self, context: &str, offer: Option<&AdapterOffer>) -> Result<Records> {
        if let Some(offer) = offer {
            crate::contexts::consider_offer(&self.store, context, offer).map_err(Error::Protocol)?;
        }
        crate::contexts::adapter(&self.store, context)
            .ok_or_else(|| no_adapter(context, offer.is_some_and(|o| o.for_this_system().is_some())))?
            .pull()
            .map_err(Error::Protocol)
    }

    fn context_push(&self, context: &str, changes: Vec<RecordChange>) -> Result<Option<String>> {
        crate::contexts::adapter(&self.store, context).ok_or_else(|| no_adapter(context, false))?.push(changes).map_err(Error::Protocol)
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
