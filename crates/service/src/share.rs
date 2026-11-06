//! Store and forward through the person's own cloud share (docs/DESIGN.md
//! section 10, transport 3): `droidtop-agent share set <folder>` names a
//! folder their own sync tool (Drive, OneDrive, Dropbox, Nextcloud, a
//! Syncthing folder they already run) carries to the handheld, and
//! `droidtop-agent share webdav <url>` a WebDAV share both reach directly
//! (crate::webdav, with the `webdav` feature). Once a minute the running
//! agent opens what paired devices left for it and leaves them the library
//! changes they have not had. Both kinds are a [`Share`], so the letters and
//! their sealing are the same on either.

use droidtop_agent_core::sync::LockExt;
use std::fs;

use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::mailbox::{self, Envelope};
use droidtop_agent_core::manifest::{self, FileEntry, Manifest};
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves;
use droidtop_agent_core::savesync::keep_copy;
use droidtop_agent_core::share::{Folder, Share};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::Agent;

/// The share set on this computer: the WebDAV share when there is one, else the folder.
pub fn open(agent: &Agent) -> Result<Option<Box<dyn Share>>, String> {
    #[cfg(feature = "webdav")]
    if let Some(dav) = crate::webdav::current(agent).map_err(|e| format!("the WebDAV share's settings: {e}"))? {
        return Ok(Some(Box::new(crate::webdav::connect(agent, &dav).map_err(|e| e.to_string())?)));
    }
    let Some(folder) = agent.store.settings.locked().share.clone() else { return Ok(None) };
    if !folder.is_dir() {
        return Err(format!("{} is not there", folder.display()));
    }
    Ok(Some(Box::new(Folder::new(folder))))
}

pub fn process(agent: &Agent) -> Result<(), String> {
    let Some(share) = open(agent)? else { return Ok(()) };
    process_with(agent, share.as_ref())
}

/// [`process`] on a share the caller opened.
pub fn process_with(agent: &Agent, share: &dyn Share) -> Result<(), String> {
    let (letters, failed) = mailbox::collect_via(share, &agent.store.key, |p| agent.store.is_trusted(p)).map_err(|e| e.to_string())?;
    for (path, why) in failed {
        agent.events.warn("share", format!("Left {path} in the share: {why}"), json!({ "path": path }));
    }
    for letter in letters {
        match mailbox::unpack(&letter.payload) {
            Ok((envelope, contents)) => handle(agent, share, &letter.from, envelope, contents),
            Err(e) => agent.events.warn(
                "share",
                format!("A message from {} could not be read: {e}", agent.store.device_name(&letter.from)),
                json!({ "peer": letter.from.to_hex() }),
            ),
        }
        if let Err(e) = share.remove(&letter.path) {
            agent.events.warn("share", format!("Could not remove {} from the share: {e}", letter.path), json!({ "path": letter.path }));
        }
    }
    send_library(agent, share)
}

fn handle(agent: &Agent, share: &dyn Share, from: &PeerId, envelope: Envelope, contents: &[u8]) {
    match envelope {
        Envelope::Library { changes } => {
            let changed = agent.store.edit_library(|lib| {
                let changed = changes.into_iter().filter(|c| lib.apply(c.clone())).count();
                (changed, changed > 0)
            });
            if changed > 0 {
                let device = agent.store.device_name(from);
                agent.events.info(
                    "library",
                    format!("Library: {changed} changes from {device} through the cloud share."),
                    json!({ "peer": from.to_hex(), "device": device, "received": changed }),
                );
            }
        }
        Envelope::Saves { game, base, files } => {
            let sender = agent.store.device_name(from);
            let detail = json!({ "peer": from.to_hex(), "device": sender, "title": game.title, "key": game.key });
            match apply_saves(agent, &game, base, &files, contents, &sender) {
                Ok(true) => agent.events.info("saves", format!("Saves of {} from {sender} through the cloud share.", game.title), detail),
                Ok(false) => {}
                Err(reason) => {
                    agent.events.warn("saves", format!("Saves of {} from {sender} not taken: {reason}", game.title), detail);
                    let reply = Envelope::SavesRefused { game, reason };
                    if let Ok(payload) = mailbox::pack(&reply, &[]) {
                        let _ = mailbox::post_via(share, &agent.store.key, from, &payload);
                    }
                }
            }
        }
        Envelope::SavesRefused { game, reason } => {
            let device = agent.store.device_name(from);
            agent.events.warn(
                "saves",
                format!("{device} could not take the saves of {}: {reason}", game.title),
                json!({ "peer": from.to_hex(), "device": device, "title": game.title, "key": game.key, "error": reason }),
            );
        }
    }
}

/// Applies a save set left in the share. When this computer's saves changed
/// too since the set's baseline, nobody wins: nothing here changes, the
/// handheld's set is kept as its device's copy under [`sender`], and a
/// refusal goes back to it; the next live sync asks the person which set to
/// keep. Ok(false) when the set was already here.
fn apply_saves(
    agent: &Agent,
    game: &GameRef,
    base: Vec<FileEntry>,
    files: &[FileEntry],
    contents: &[u8],
    sender: &str,
) -> Result<bool, String> {
    let plan = agent.save_plan(game, false).ok_or("this computer knows no save location for the game")?;
    if plan.all_by_syncthing() {
        return Err("Syncthing already syncs this game's saves on this computer".into());
    }
    let (spec, roots) = (plan.spec, plan.roots);
    let current = manifest::build(saves::collect(&spec, &roots), &Manifest::new());
    let mut offset = 0usize;
    let mut blobs = Vec::new();
    for f in files {
        let end = offset + f.size as usize;
        let blob = contents.get(offset..end).ok_or("the save set arrived cut short")?;
        if droidtop_agent_core::hex::encode(&Sha256::digest(blob)) != f.sha256 || !spec.matches(&f.name) {
            return Err(format!("{} arrived damaged or is not one of the game's saves", f.name));
        }
        blobs.push(blob);
        offset = end;
    }
    let incoming = manifest::from_entries(files.to_vec());
    // Already the same here (a live sync got there first): nothing to do.
    if manifest::same(&current, &incoming) {
        return Ok(false);
    }
    let archive = agent.store.dirs.archive().join(droidtop_agent_saves::file_key(&game.key));
    if !manifest::same(&current, &manifest::from_entries(base)) {
        // This computer's saves stay; the handheld's are kept beside them.
        let tmp = std::env::temp_dir().join(format!("droidtop-agent-incoming-{}", std::process::id()));
        let mut staged = Vec::new();
        for (f, blob) in files.iter().zip(&blobs) {
            let p = tmp.join(manifest::key(&f.name).replace(['<', '>', '/'], "_"));
            fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
            fs::write(&p, blob).map_err(|e| e.to_string())?;
            staged.push((f.name.clone(), p));
        }
        keep_copy(staged.into_iter(), &archive, sender).map_err(|e| e.to_string())?;
        if tmp.starts_with(std::env::temp_dir()) {
            let _ = fs::remove_dir_all(&tmp);
        }
        return Err(format!(
            "this computer's saves changed too, so nothing was overwritten; yours are kept on it in {}, and your next sync with it asks which to keep",
            archive.display()
        ));
    }
    for (f, blob) in files.iter().zip(blobs) {
        let path = saves::local_path(&f.name, &roots).ok_or("no place for a file")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, blob).map_err(|e| e.to_string())?;
        if let Ok(file) = fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(manifest::system_time(f.mtime_ms));
        }
    }
    for (k, e) in &current {
        if !incoming.contains_key(k) {
            if let Some(p) = saves::local_path(&e.name, &roots) {
                let _ = fs::remove_file(p);
            }
        }
    }
    Ok(true)
}

/// Leaves each paired device the library changes it has not been sent.
fn send_library(agent: &Agent, share: &dyn Share) -> Result<(), String> {
    let devices: Vec<String> = agent.store.devices_now().iter().map(|d| d.id.clone()).collect();
    for id in devices {
        let Ok(peer) = PeerId::from_hex(&id) else { continue };
        let (changes, seq) = {
            let lib = agent.store.library_now();
            let pushed = lib.cursors.get(&id).map(|c| c.pushed).unwrap_or(0);
            lib.changes_since(pushed)
        };
        // Whoever opens a letter may run an older library that cannot read a
        // link (crate::library::LINK_FEATURE), and a letter has no hello to
        // say so: links travel only in live sessions.
        let changes: Vec<_> = changes.into_iter().filter(droidtop_agent_core::library::Change::readable_without_links).collect();
        if changes.is_empty() {
            continue;
        }
        let payload = mailbox::pack(&Envelope::Library { changes }, &[]).map_err(|e| e.to_string())?;
        mailbox::post_via(share, &agent.store.key, &peer, &payload).map_err(|e| e.to_string())?;
        agent.store.edit_library(|lib| {
            lib.cursors.entry(id).or_default().pushed = seq;
            ((), true)
        });
    }
    Ok(())
}
