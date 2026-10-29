//! The side that receives an offer (docs/DESIGN.md section 20): this
//! computer reaches a paired device that offers it something, browses the
//! games it offers, accepts one (copied here as a new version folder), or
//! asks that device to play one. Nothing here is offered to anyone; it only
//! asks, and the other device decides what it answers.

use droidtop_agent_core::sync::LockExt;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use droidtop_agent_core::client::{self, ConnectError, RendezvousSettings, Session, Target};
use droidtop_agent_core::gamecopy::{self, Copied};
use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::library::Library;
use droidtop_agent_core::proto::{GameRef, Launch, Request, Response, NOT_OFFERED};
use droidtop_agent_core::PORT;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Agent;

/// What a device offers this computer, as its hello said.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Offered {
    pub library: bool,
    pub saves: bool,
    /// It can start games for this device (whether it allows it is its own
    /// switch, answered when asked).
    pub launch: bool,
}

/// One game a device offers: its own install there.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct OfferedGame {
    pub key: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub size: u64,
    /// The person's marks on it (favourite, collections...).
    pub marks: std::collections::BTreeMap<String, serde_json::Value>,
    /// This computer has accepted it from that device before.
    pub accepted: bool,
}

/// What a browse found.
#[derive(Serialize, Debug, Clone)]
pub struct Browse {
    /// The device's name, as it said.
    pub name: String,
    pub offered: Offered,
    /// Its games, by title; empty when it does not offer its library.
    pub games: Vec<OfferedGame>,
}

/// A game this computer accepted from a device.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub key: String,
    pub title: String,
    /// The device it came from (id hex).
    pub from: String,
    pub folder: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub at_ms: i64,
    /// Keep its saves and versions in step with that device; off until the
    /// person turns it on (owner, 2026-10-23).
    #[serde(default)]
    pub keep_in_step: bool,
}

fn refusal(e: ConnectError) -> String {
    match e {
        ConnectError::Unreachable(m) => format!("It could not be reached: {m}."),
        ConnectError::Failed(e) => e.to_string(),
    }
}

impl Agent {
    /// Where the accepted games are recorded.
    fn accepted_path(&self) -> PathBuf {
        self.store.dirs.data.join("accepted.json")
    }

    /// The games this computer accepted from other devices, newest first.
    pub fn accepted(&self) -> Vec<Accepted> {
        let mut all: Vec<Accepted> = fs::read(self.accepted_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        all.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
        all
    }

    fn note_accepted(&self, one: Accepted) -> std::io::Result<()> {
        let mut all = self.accepted();
        all.retain(|a| a.folder != one.folder);
        all.push(one);
        let path = self.accepted_path();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
        fs::rename(tmp, path)
    }

    /// Turns "keep in step" on or off for a game accepted into [`folder`].
    pub fn set_keep_in_step(&self, folder: &std::path::Path, on: bool) -> Result<(), String> {
        let mut all = self.accepted();
        let one = all.iter_mut().find(|a| a.folder == folder).ok_or("This computer did not accept a game into that folder.")?;
        one.keep_in_step = on;
        let path = self.accepted_path();
        fs::write(&path, serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }

    /// A session with the paired device [`peer`], by the paths the handheld
    /// uses: where it was last seen, the LAN, and a rendezvous when that is on.
    fn reach(&self, peer: &PeerId) -> Result<Session, String> {
        let id = peer.to_hex();
        let device = self.store.devices_now().into_iter().find(|d| d.id == id).ok_or("That device is not paired with this computer.")?;
        // Where it was last seen: where it answered when this computer reached
        // it, or where it connected from when it reached this one; the agent's
        // port on that host covers the second.
        let mut addresses: Vec<String> = Vec::new();
        if let Some(seen) = device.last_address.as_deref().and_then(|a| a.parse::<SocketAddr>().ok()) {
            addresses.push(seen.to_string());
            let listening = SocketAddr::new(seen.ip(), PORT).to_string();
            if !addresses.contains(&listening) {
                addresses.push(listening);
            }
        }
        let settings = self.store.settings.locked().clone();
        let rendezvous = settings.rendezvous.then(|| RendezvousSettings {
            servers: settings.discovery_servers.clone(),
            stun: settings.stun_servers.clone(),
            state: self.store.dirs.data.join("rendezvous-client.json"),
        });
        let name = self.store.name();
        let target =
            Target { peer: *peer, name: &name, addresses: &addresses, disco: device.disco.as_deref(), rendezvous: rendezvous.as_ref() };
        let session = client::connect(&self.store.key, &target).map_err(refusal)?;
        if !session.address.is_empty() {
            self.store.seen(peer, Some(session.address.clone()));
        }
        Ok(session)
    }

    /// What [`peer`] offers this computer, and the games it offers.
    pub fn browse(&self, peer: &PeerId) -> Result<Browse, String> {
        let mut s = self.reach(peer)?;
        let offered = Offered { library: s.states("offers_library"), saves: s.states("offers_saves"), launch: s.states("launch") };
        let mut games = Vec::new();
        if offered.library {
            s.ch.send_json(&Request::LibraryPull { since: 0 }).map_err(|e| e.to_string())?;
            let changes = match s.ch.recv_json::<Response>().map_err(|e| e.to_string())? {
                Response::LibraryChanges { changes, .. } => changes,
                Response::Error { message, .. } => return Err(message),
                other => return Err(format!("It answered {other:?}.")),
            };
            let mut lib = Library::default();
            for c in changes {
                lib.apply(c);
            }
            let theirs = peer.to_hex();
            let accepted = self.accepted();
            for (key, g) in &lib.games {
                let Some(install) = g.installs.get(&theirs).map(|i| &i.install).filter(|i| i.installed) else { continue };
                games.push(OfferedGame {
                    key: key.clone(),
                    title: g.title.clone(),
                    platform: g.platform.clone(),
                    version: install.version.clone(),
                    size: install.size,
                    marks: g.marks.iter().map(|(field, m)| (field.clone(), m.value.clone())).collect(),
                    accepted: accepted.iter().any(|a| a.key == *key && a.from == theirs),
                });
            }
            games.sort_by_key(|g| g.title.to_lowercase());
        }
        let name = s.name.clone();
        s.bye();
        Ok(Browse { name, offered, games })
    }

    /// Copies [`game`] from [`peer`] into this computer's first game folder,
    /// as a new folder named for it and its version, then rescans.
    pub fn accept(&self, peer: &PeerId, game: &GameRef, progress: &mut dyn gamecopy::Progress) -> Result<Accepted, String> {
        let parent = self
            .store
            .settings
            .lock()
            .unwrap()
            .game_folders
            .first()
            .cloned()
            .ok_or("Add a game folder on this computer first; an accepted game is copied into it.")?;
        let mut s = self.reach(peer)?;
        if !s.states("offers_library") {
            s.bye();
            return Err(format!("{} does not offer its library to this computer.", self.store.device_name(peer)));
        }
        s.ch.send_json(&Request::GameFiles { game: game.clone() }).map_err(|e| e.to_string())?;
        let folder = match s.ch.recv_json::<Response>().map_err(|e| e.to_string())? {
            Response::GameFiles { folder } => folder,
            Response::Error { message, .. } => return Err(message),
            other => return Err(format!("It answered {other:?}.")),
        };
        let name = free_name(&parent, &folder.name, folder.version.as_deref(), &game.title);
        let copied: Copied = gamecopy::pull(&mut s.ch, game, &parent, &name, progress).map_err(|e| e.to_string())?;
        s.bye();
        let one = Accepted {
            key: game.key.clone(),
            title: game.title.clone(),
            from: peer.to_hex(),
            folder: copied.folder.clone(),
            version: folder.version.clone(),
            at_ms: droidtop_agent_core::library::now_ms(),
            keep_in_step: false,
        };
        self.note_accepted(one.clone()).map_err(|e| e.to_string())?;
        self.rescan();
        self.events.info(
            "accept",
            format!("Copied {} from {} ({} files).", game.title, self.store.device_name(peer), copied.files),
            json!({ "peer": peer.to_hex(), "key": game.key, "folder": copied.folder, "files": copied.files, "bytes": copied.bytes }),
        );
        Ok(one)
    }

    /// Asks [`peer`] to start [`game`] there and stream it (section 17).
    pub fn play_on(&self, peer: &PeerId, game: &GameRef) -> Result<Launch, String> {
        let mut s = self.reach(peer)?;
        if !s.states("launch") {
            s.bye();
            return Err("That device does not start games for others.".into());
        }
        s.ch.send_json(&Request::LaunchGame { game: game.clone() }).map_err(|e| e.to_string())?;
        let outcome = match s.ch.recv_json::<Response>().map_err(|e| e.to_string())? {
            Response::Launch { outcome } => outcome,
            Response::Error { message, code } if code.as_deref() == Some(NOT_OFFERED) => return Err(message),
            Response::Error { message, .. } => return Err(message),
            other => return Err(format!("It answered {other:?}.")),
        };
        s.bye();
        Ok(outcome)
    }
}

/// A folder name in [`parent`] that is free: the other device's own name for
/// it, or else the title and version, with " (2)" and on when taken.
fn free_name(parent: &std::path::Path, theirs: &str, version: Option<&str>, title: &str) -> String {
    let base = if gamecopy::safe_folder_name(theirs) {
        theirs.to_string()
    } else {
        let made: String = match version {
            Some(v) => format!("{title} {v}"),
            None => title.to_string(),
        }
        .chars()
        .map(|c| if c.is_alphanumeric() || " .-_()".contains(c) { c } else { '_' })
        .collect();
        made.trim().trim_matches('.').to_string()
    };
    let base = if base.is_empty() { "Game".to_string() } else { base };
    let mut name = base.clone();
    let mut n = 2;
    while parent.join(&name).exists() || parent.join(format!("{name}{}", gamecopy::PART)).exists() {
        name = format!("{base} ({n})");
        n += 1;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accepted_folder_never_lands_on_one_already_there() {
        let dir = std::env::temp_dir().join(format!("dta-free-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Hades v1.0")).unwrap();
        assert_eq!(free_name(&dir, "Hades v1.0", Some("1.0"), "Hades"), "Hades v1.0 (2)");
        assert_eq!(free_name(&dir, "../up", Some("1.1"), "Hades: Deluxe"), "Hades_ Deluxe 1.1");
        assert_eq!(free_name(&dir, "", None, ".."), "Game");
        let _ = fs::remove_dir_all(&dir);
    }
}
