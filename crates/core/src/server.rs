//! The computer's side of a session: one loop that answers the handheld's
//! requests from a [`Host`], which the agent implements with its scanner,
//! save catalog, library and context adapters.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::channel::Channel;
use crate::context::{AdapterOffer, RecordChange, Records};
use crate::keys::PeerId;
use crate::library::Change;
use crate::manifest::{self, Manifest};
use crate::proto::{GameRef, Request, Response};
use crate::saves::{self, Roots, SaveSpec};
use crate::savesync::{archive_files, entry_for, receive_file, send_contents};
use crate::{Error, Result, FEATURES, PROTOCOL_VERSION};

pub trait Host: Send + Sync {
    /// This computer's name, as the handheld shows it.
    fn name(&self) -> String;
    /// Where this computer's WireGuard answers from outside the LAN, as `wg:<ip>:<port>`.
    fn endpoints(&self) -> Vec<String> {
        Vec::new()
    }
    /// This computer's global discovery ID, when it takes part in rendezvous.
    fn disco_id(&self) -> Option<String> {
        None
    }
    /// A handheld said hello, with its global discovery ID when it has one.
    fn hello(&self, _peer: &PeerId, _disco: Option<&str>) {}
    /// Where [`game`] keeps its saves, and this computer's folder for each token.
    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)>;
    /// Where this computer's conflict loser for [`game`] goes.
    fn archive_dir(&self, game: &GameRef) -> PathBuf;
    fn library_pull(&self, peer: &PeerId, since: u64) -> Result<(Vec<Change>, u64)>;
    fn library_push(&self, peer: &PeerId, changes: Vec<Change>) -> Result<()>;
    /// A context's records here. [`offer`] is the adapter the plugin offers,
    /// for a computer that has none for [`context`] yet.
    fn context_pull(&self, context: &str, offer: Option<&AdapterOffer>) -> Result<Records>;
    /// Applies the changes; Ok(Some(reason)) when they must wait (the app that
    /// owns the data is running).
    fn context_push(&self, context: &str, changes: Vec<RecordChange>) -> Result<Option<String>>;
}

/// Answers requests until the handheld says goodbye or hangs up.
pub fn serve<S: Read + Write>(ch: &mut Channel<S>, host: &dyn Host) -> Result<()> {
    let peer = ch.peer();
    let mut specs: HashMap<GameRef, Option<(SaveSpec, Roots)>> = HashMap::new();
    let mut known: HashMap<GameRef, Manifest> = HashMap::new();
    loop {
        let request: Request = match ch.recv_json() {
            Ok(r) => r,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut spec_of = |game: &GameRef| specs.entry(game.clone()).or_insert_with(|| host.saves(game)).clone();
        let reply = match request {
            Request::Bye => return Ok(()),
            Request::Hello { disco, .. } => {
                host.hello(&peer, disco.as_deref());
                Ok(Response::Hello {
                    name: host.name(),
                    version: PROTOCOL_VERSION,
                    features: FEATURES.iter().map(|f| f.to_string()).collect(),
                    endpoints: host.endpoints(),
                    disco: host.disco_id(),
                })
            }
            Request::LibraryPull { since } => {
                host.library_pull(&peer, since).map(|(changes, cursor)| Response::LibraryChanges { changes, cursor })
            }
            Request::LibraryPush { changes } => host.library_push(&peer, changes).map(|()| Response::Ok),
            Request::SaveSpec { game } => Ok(Response::SaveSpec { spec: spec_of(&game).map(|(s, _)| s) }),
            Request::SaveManifest { game } => match spec_of(&game) {
                Some((spec, roots)) => {
                    let m = manifest::build(saves::collect(&spec, &roots), known.get(&game).unwrap_or(&Manifest::new()));
                    let files = m.values().cloned().collect();
                    known.insert(game, m);
                    Ok(Response::Manifest { files })
                }
                None => Ok(Response::Manifest { files: Vec::new() }),
            },
            Request::FileGet { game, name } => match spec_of(&game) {
                Some((spec, roots)) if spec.matches(&name) => match saves::local_path(&name, &roots).filter(|p| p.is_file()) {
                    // The header states the digest, so the file is read
                    // now; a change under it fails the handheld's check.
                    Some(path) => match entry_for(&name, &path) {
                        Ok(file) => {
                            ch.send_json(&Response::File { file: file.clone() })?;
                            send_contents(ch, &path, file.size)?;
                            continue;
                        }
                        Err(e) => Err(e),
                    },
                    None => Err(Error::Protocol(format!("{name} is not on this computer"))),
                },
                _ => Err(Error::Protocol(format!("{name} is not one of this game's saves"))),
            },
            Request::FilePut { game, file } => {
                // The contents follow the header whatever happens, so they
                // are read in full before a refusal is sent.
                let target = match spec_of(&game) {
                    Some((spec, roots)) if spec.matches(&file.name) => saves::local_path(&file.name, &roots),
                    _ => None,
                };
                match target {
                    Some(target) => receive_file(ch, &file, &target).map(|()| Response::Ok),
                    None => {
                        let mut left = file.size;
                        while left > 0 {
                            left = left.saturating_sub(ch.recv()?.len() as u64);
                        }
                        Err(Error::Protocol(format!("{} is not one of this game's saves", file.name)))
                    }
                }
            }
            Request::SaveApply { game, remove, archive } => match spec_of(&game) {
                Some((spec, roots)) => apply(host, &game, &spec, &roots, &remove, archive).map(|()| Response::Ok),
                None => Err(Error::Protocol("this computer knows no saves for that game".into())),
            },
            Request::ContextPull { context, adapter } => {
                host.context_pull(&context, adapter.as_ref()).map(|records| Response::Context { records })
            }
            Request::ContextPush { context, changes } => host
                .context_push(&context, changes)
                .map(|deferred| Response::ContextApplied { deferred: deferred.is_some(), message: deferred }),
        };
        let reply = reply.unwrap_or_else(|e| Response::Error { message: e.to_string() });
        ch.send_json(&reply)?;
    }
}

fn apply(host: &dyn Host, game: &GameRef, spec: &SaveSpec, roots: &Roots, remove: &[String], archive: bool) -> Result<()> {
    if archive {
        archive_files(saves::collect(spec, roots).into_iter(), &host.archive_dir(game))?;
    }
    for name in remove {
        if !spec.matches(name) {
            continue;
        }
        if let Some(path) = saves::local_path(name, roots) {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
