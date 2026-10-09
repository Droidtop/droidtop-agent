//! Store and forward through the person's own cloud share (docs/DESIGN.md
//! section 10, transport 3): `droidtop-agent share set <folder>` names a
//! folder their own sync tool (Drive, OneDrive, Dropbox, Nextcloud, a
//! Syncthing folder they already run) carries to the handheld. Once a
//! minute the running agent opens what paired devices left for it and
//! leaves them the library changes they have not had.

use std::fs;

use droidtop_agent_core::keys::PeerId;
use droidtop_agent_core::mailbox::{self, Envelope};
use droidtop_agent_core::manifest::{self, FileEntry, Manifest};
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves;
use droidtop_agent_core::savesync::archive_files;
use sha2::{Digest, Sha256};

use crate::state::Agent;

pub fn process(agent: &Agent) -> Result<(), String> {
    let Some(share) = agent.settings.lock().unwrap().share.clone() else { return Ok(()) };
    if !share.is_dir() {
        return Err(format!("{} is not there", share.display()));
    }
    let (letters, failed) = mailbox::collect(&share, &agent.key, |p| agent.is_trusted(p)).map_err(|e| e.to_string())?;
    for (path, why) in failed {
        eprintln!("Left {} in the share: {why}", path.display());
    }
    for letter in letters {
        match mailbox::unpack(&letter.payload) {
            Ok((envelope, contents)) => handle(agent, &share, &letter.from, envelope, contents),
            Err(e) => eprintln!("A message from {} could not be read: {e}", letter.from),
        }
        let _ = fs::remove_file(&letter.path);
    }
    send_library(agent, &share)
}

fn handle(agent: &Agent, share: &std::path::Path, from: &PeerId, envelope: Envelope, contents: &[u8]) {
    match envelope {
        Envelope::Library { changes } => {
            let changed = {
                let mut lib = agent.library.lock().unwrap();
                changes.into_iter().filter(|c| lib.apply(c.clone())).count()
            };
            if changed > 0 {
                let _ = agent.save_library();
            }
        }
        Envelope::Saves { game, base, files } => {
            if let Err(reason) = apply_saves(agent, &game, base, &files, contents) {
                let reply = Envelope::SavesRefused { game, reason };
                if let Ok(payload) = mailbox::pack(&reply, &[]) {
                    let _ = mailbox::post(share, &agent.key, from, &payload);
                }
            }
        }
        Envelope::SavesRefused { game, reason } => eprintln!("{} could not take the saves of {}: {reason}", from, game.title),
    }
}

/// Applies a save set left in the share when this computer's saves still
/// match the baseline it was made against; otherwise keeps it in the
/// archive and says why.
fn apply_saves(agent: &Agent, game: &GameRef, base: Vec<FileEntry>, files: &[FileEntry], contents: &[u8]) -> Result<(), String> {
    let (spec, roots, _) = agent.save_lookup(game, false).ok_or("this computer knows no save location for the game")?;
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
    // Already the same here (a live sync got there first): nothing to do,
    // and nothing to archive.
    if manifest::same(&current, &manifest::from_entries(files.to_vec())) {
        return Ok(());
    }
    let archive = agent.dirs.archive().join(crate::host::file_key(&game.key));
    if !manifest::same(&current, &manifest::from_entries(base)) {
        // Both changed: the incoming set is kept, not applied.
        let tmp = std::env::temp_dir().join(format!("droidtop-agent-incoming-{}", std::process::id()));
        let mut staged = Vec::new();
        for (f, blob) in files.iter().zip(&blobs) {
            let p = tmp.join(manifest::key(&f.name).replace(['<', '>', '/'], "_"));
            fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
            fs::write(&p, blob).map_err(|e| e.to_string())?;
            staged.push((f.name.clone(), p));
        }
        archive_files(staged.into_iter(), &archive).map_err(|e| e.to_string())?;
        if tmp.starts_with(std::env::temp_dir()) {
            let _ = fs::remove_dir_all(&tmp);
        }
        return Err(format!("the saves changed on this computer too; the handheld's copy is in {}", archive.display()));
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
    let incoming = manifest::from_entries(files.to_vec());
    for (k, e) in &current {
        if !incoming.contains_key(k) {
            if let Some(p) = saves::local_path(&e.name, &roots) {
                let _ = fs::remove_file(p);
            }
        }
    }
    Ok(())
}

/// Leaves each paired device the library changes it has not been sent.
fn send_library(agent: &Agent, share: &std::path::Path) -> Result<(), String> {
    let devices: Vec<String> = agent.devices.lock().unwrap().iter().map(|d| d.id.clone()).collect();
    for id in devices {
        let Ok(peer) = PeerId::from_hex(&id) else { continue };
        let (changes, seq) = {
            let lib = agent.library.lock().unwrap();
            let pushed = lib.cursors.get(&id).map(|c| c.pushed).unwrap_or(0);
            let (changes, seq) = lib.changes_since(pushed);
            (changes, seq)
        };
        if changes.is_empty() {
            continue;
        }
        let payload = mailbox::pack(&Envelope::Library { changes }, &[]).map_err(|e| e.to_string())?;
        mailbox::post(share, &agent.key, &peer, &payload).map_err(|e| e.to_string())?;
        agent.library.lock().unwrap().cursors.entry(id).or_default().pushed = seq;
        agent.save_library().map_err(|e| e.to_string())?;
    }
    Ok(())
}
