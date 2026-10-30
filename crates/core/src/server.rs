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
use crate::proto::{check_announcements, Announcements, GameRef, Launch, Offer, Request, Response, NOT_OFFERED};
use crate::saves::{self, Roots, SaveSpec};
use crate::savesync::{entry_for, keep_copy, receive_file, send_contents};
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
    /// What this computer can do beyond the core's [`FEATURES`], stated in
    /// `hello` after them (`launch_host`: it starts games other than Steam's).
    fn extra_features(&self) -> Vec<String> {
        Vec::new()
    }
    /// What this device offers [`peer`] (docs/DESIGN.md section 20), read
    /// again at each request it gates, so a change holds at once.
    fn offers(&self, peer: &PeerId) -> Offer;
    /// Whether [`peer`] may sync plugin contexts here (section 8): never part
    /// of an offer.
    fn syncs_contexts(&self, peer: &PeerId) -> bool;
    /// Where [`game`] keeps its saves, and this computer's folder for each token.
    fn saves(&self, game: &GameRef) -> Option<(SaveSpec, Roots)>;
    /// [`game`]'s archive here: the copies kept per device, and older ones.
    fn archive_dir(&self, game: &GameRef) -> PathBuf;
    /// [`game`]'s folder on this computer and the version it has, for a
    /// handheld that copies it (crate::gamecopy).
    fn game_folder(&self, _game: &GameRef) -> Option<(PathBuf, Option<String>)> {
        None
    }
    /// Where a game folder a handheld sends is made: one of the person's game
    /// folders here, or why there is none.
    fn game_inbox(&self) -> std::result::Result<PathBuf, String> {
        Err("this computer takes no games".into())
    }
    /// A game folder a handheld sent is in place.
    fn game_arrived(&self, _game: &GameRef, _folder: &std::path::Path) {}
    /// At the end of a session in which [`peer`] synced [`game`]'s saves:
    /// how many save files this computer sent and received, and how many it
    /// removed because the handheld's set no longer has them.
    fn saves_synced(&self, _peer: &PeerId, _game: &GameRef, _sent: usize, _received: usize, _removed: usize) {}
    /// [`peer`] announced [`theirs`] (every plugin's value, in place of
    /// what it announced before, already checked); answers this device's own.
    fn announce(&self, _peer: &PeerId, _theirs: Announcements) -> Result<Announcements> {
        Err(Error::Protocol("this device keeps no announcements".into()))
    }
    /// Starts [`game`] for [`peer`] (hello feature `launch`). A computer that
    /// cannot start games says so; the agent starts Steam games only, and
    /// only for a handheld the person allowed (docs/DESIGN.md section 17).
    fn launch_game(&self, _peer: &PeerId, _game: &GameRef) -> Launch {
        Launch::Failed { message: "this computer cannot start games".into() }
    }
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
    serve_moved(ch, host, None)
}

/// [`serve`] for a session the handheld opened to an identity this computer
/// is moving away from: its hello reply carries [`moved`].
pub fn serve_moved<S: Read + Write>(ch: &mut Channel<S>, host: &dyn Host, moved: Option<crate::moved::Moved>) -> Result<()> {
    let peer = ch.peer();
    let mut synced = HashMap::new();
    let result = session(ch, host, moved, &mut synced);
    for (game, Synced { sent, received, removed }) in synced {
        host.saves_synced(&peer, &game, sent, received, removed);
    }
    result
}

/// Save files a session moved for one game.
#[derive(Default)]
struct Synced {
    sent: usize,
    received: usize,
    removed: usize,
}

fn session<S: Read + Write>(
    ch: &mut Channel<S>,
    host: &dyn Host,
    moved: Option<crate::moved::Moved>,
    synced: &mut HashMap<GameRef, Synced>,
) -> Result<()> {
    let peer = ch.peer();
    let mut specs: HashMap<GameRef, Option<(SaveSpec, Roots)>> = HashMap::new();
    let mut known: HashMap<GameRef, Manifest> = HashMap::new();
    let mut incoming: Option<(GameRef, crate::gamecopy::Incoming)> = None;
    loop {
        let request: Request = match ch.recv_json() {
            Ok(r) => r,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Some(refusal) = refusal(host, &peer, &request) {
            // A save file's contents follow its header whatever happens.
            if let Request::FilePut { file, .. } = &request {
                let mut left = file.size;
                while left > 0 {
                    left = left.saturating_sub(ch.recv()?.len() as u64);
                }
            }
            ch.send_json(&refusal)?;
            continue;
        }
        let mut spec_of = |game: &GameRef| specs.entry(game.clone()).or_insert_with(|| host.saves(game)).clone();
        let reply = match request {
            Request::Bye => return Ok(()),
            Request::Hello { disco, .. } => {
                host.hello(&peer, disco.as_deref());
                Ok(Response::Hello {
                    name: host.name(),
                    version: PROTOCOL_VERSION,
                    features: features(host, &peer),
                    endpoints: host.endpoints(),
                    disco: host.disco_id(),
                    moved_to: moved.clone(),
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
                            synced.entry(game).or_default().sent += 1;
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
                    Some(target) => receive_file(ch, &file, &target).map(|()| {
                        synced.entry(game).or_default().received += 1;
                        Response::Ok
                    }),
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
                Some((spec, roots)) => apply(host, &game, &spec, &roots, &remove, archive).map(|removed| {
                    synced.entry(game).or_default().removed += removed;
                    Response::Ok
                }),
                None => Err(Error::Protocol("this computer knows no saves for that game".into())),
            },
            Request::GameFiles { game } => match host.game_folder(&game) {
                Some((folder, version)) => crate::gamecopy::describe(&folder, version).map(|folder| Response::GameFiles { folder }),
                None => Err(Error::Protocol(format!("{} is not installed on this computer", game.title))),
            },
            Request::GameFileGet { game, path, offset } => match host.game_folder(&game) {
                Some((folder, _)) => match crate::gamecopy::send_file(ch, &folder, &path, offset) {
                    Ok(()) => continue,
                    Err(e) => Err(e),
                },
                None => Err(Error::Protocol(format!("{} is not installed on this computer", game.title))),
            },
            Request::GamePutStart { game, folder } => match host.game_inbox() {
                Ok(parent) => crate::gamecopy::Incoming::start(&parent, folder).map(|(inc, have)| {
                    incoming = Some((game, inc));
                    Response::GamePutHave { have }
                }),
                Err(why) => Err(Error::Protocol(why)),
            },
            Request::GameFilePut { game, path } => match &incoming {
                // The file follows whatever happens; a refusal comes after it.
                Some((g, inc)) if *g == game => inc.receive(ch, &path).map(|()| Response::Ok),
                _ => Err(Error::Protocol("no game folder is being sent".into())),
            },
            Request::GamePutFinish { game } => match incoming.take() {
                Some((g, inc)) if g == game => inc.finish().map(|folder| {
                    host.game_arrived(&game, &folder);
                    Response::GamePlaced { folder: folder.display().to_string() }
                }),
                _ => Err(Error::Protocol("no game folder is being sent".into())),
            },
            Request::LaunchGame { game } => Ok(Response::Launch { outcome: host.launch_game(&peer, &game) }),
            Request::Announce { values } => match check_announcements(&values) {
                Ok(()) => host.announce(&peer, values).map(|values| Response::Announced { values }),
                Err(why) => Err(Error::Protocol(why)),
            },
            Request::ContextPull { context, adapter } => {
                host.context_pull(&context, adapter.as_ref()).map(|records| Response::Context { records })
            }
            Request::ContextPush { context, changes } => host
                .context_push(&context, changes)
                .map(|deferred| Response::ContextApplied { deferred: deferred.is_some(), message: deferred }),
        };
        let reply = reply.unwrap_or_else(|e| Response::Error { message: e.to_string(), code: None });
        ch.send_json(&reply)?;
    }
}

/// The core's features, what this device offers [`peer`]
/// (`offers_library`, `offers_saves`), then the host's own.
fn features(host: &dyn Host, peer: &PeerId) -> Vec<String> {
    let offer = host.offers(peer);
    let offered = [("offers_library", offer.library), ("offers_saves", offer.saves)];
    FEATURES
        .iter()
        .map(|f| f.to_string())
        .chain(offered.iter().filter(|(_, on)| *on).map(|(f, _)| f.to_string()))
        .chain(host.extra_features())
        .collect()
}

/// The answer to a request this device does not offer [`peer`]: it says only
/// that, so a device that is not offered learns nothing of the games here.
/// What a device sends of its own (its library, a game folder) and starting a
/// game (section 17, its own switch) are not gated here.
fn refusal(host: &dyn Host, peer: &PeerId, request: &Request) -> Option<Response> {
    let what = match request {
        Request::LibraryPull { .. } | Request::GameFiles { .. } | Request::GameFileGet { .. } => {
            (!host.offers(peer).library).then_some("its library")
        }
        Request::SaveSpec { .. }
        | Request::SaveManifest { .. }
        | Request::FileGet { .. }
        | Request::FilePut { .. }
        | Request::SaveApply { .. } => (!host.offers(peer).saves).then_some("its saves"),
        Request::ContextPull { .. } | Request::ContextPush { .. } => (!host.syncs_contexts(peer)).then_some("plugin data"),
        _ => None,
    }?;
    Some(Response::Error { message: format!("{} does not offer {what} to this device.", host.name()), code: Some(NOT_OFFERED.into()) })
}

/// Returns how many files it removed.
fn apply(host: &dyn Host, game: &GameRef, spec: &SaveSpec, roots: &Roots, remove: &[String], archive: bool) -> Result<usize> {
    // This computer's changed set is overwritten: it keeps it as its own copy first.
    if archive {
        keep_copy(saves::collect(spec, roots).into_iter(), &host.archive_dir(game), &host.name())?;
    }
    let mut removed = 0;
    for name in remove {
        if !spec.matches(name) {
            continue;
        }
        if let Some(path) = saves::local_path(name, roots) {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(removed)
}
