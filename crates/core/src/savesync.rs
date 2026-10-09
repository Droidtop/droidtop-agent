//! Syncing one game's saves (docs/DESIGN.md sections 6 and 9).
//!
//! The rule is the one droidtop's Steam Cloud sync uses
//! (`SteamCloudPlan.decide` in droidtop's `:stores`): against the files as
//! they were at the last sync (the baseline the handheld keeps per computer
//! and game), a side that alone changed wins and its set is copied over,
//! deletions included; both changed is a conflict, and so is a first sync
//! with differing files on both sides. A conflict is the person's: the
//! handheld asks, then calls again with the side they chose, and the losing
//! side is archived before it is overwritten.
//!
//! The baseline moves file by file, so a sync cut short is finished by the
//! next one instead of turning into a conflict.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::manifest::{self, FileEntry, Manifest};
use crate::proto::{GameRef, Request, Response, CHUNK};
use crate::saves::{self, Roots, SaveSpec, PART_SUFFIX};
use crate::{error::protocol, Error, Result};

/// The device running the sync (the handheld) or the computer.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Here,
    There,
}

/// One side of a conflict, as the dialog shows it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SideSummary {
    pub files: usize,
    pub bytes: u64,
    pub newest_ms: i64,
}

fn summary(m: &Manifest) -> SideSummary {
    SideSummary { files: m.len(), bytes: m.values().map(|e| e.size).sum(), newest_ms: m.values().map(|e| e.mtime_ms).max().unwrap_or(0) }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    Nothing,
    Copy { from: Side, changed: Vec<FileEntry>, removed: Vec<String> },
    Conflict { here: SideSummary, there: SideSummary },
}

fn copy_from(side: Side, here: &Manifest, there: &Manifest) -> Plan {
    let (src, dst) = match side {
        Side::Here => (here, there),
        Side::There => (there, here),
    };
    let changed = src.iter().filter(|(k, e)| dst.get(*k).is_none_or(|d| d.sha256 != e.sha256)).map(|(_, e)| e.clone()).collect();
    let removed = dst.iter().filter(|(k, _)| !src.contains_key(*k)).map(|(_, e)| e.name.clone()).collect();
    Plan::Copy { from: side, changed, removed }
}

/// What to do, from both sides' files and the baseline.
pub fn decide(here: &Manifest, there: &Manifest, baseline: Option<&Manifest>) -> Plan {
    if manifest::same(here, there) {
        return Plan::Nothing;
    }
    let conflict = || Plan::Conflict { here: summary(here), there: summary(there) };
    match baseline {
        None if here.is_empty() => copy_from(Side::There, here, there),
        None if there.is_empty() => copy_from(Side::Here, here, there),
        None => conflict(),
        Some(base) => match (!manifest::same(here, base), !manifest::same(there, base)) {
            (true, false) => copy_from(Side::Here, here, there),
            (false, true) => copy_from(Side::There, here, there),
            _ => conflict(),
        },
    }
}

/// What a sync did.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The computer knows no save location for this game.
    NoSpec,
    UpToDate {
        files: usize,
    },
    Copied {
        from: Side,
        files: usize,
        removed: usize,
        bytes: u64,
    },
    /// Nothing changed; the person decides.
    Conflict {
        here: SideSummary,
        there: SideSummary,
    },
}

/// One sync, from the handheld's side.
pub struct SaveSyncRequest<'a> {
    pub game: GameRef,
    pub roots: &'a Roots,
    /// The baseline file for this computer and game.
    pub baseline_path: &'a Path,
    /// Where this side's conflict loser goes.
    pub archive_dir: &'a Path,
    /// The person's answer to an earlier conflict.
    pub choice: Option<Side>,
}

fn load_baseline(path: &Path) -> Option<Manifest> {
    let bytes = fs::read(path).ok()?;
    let entries: Vec<FileEntry> = serde_json::from_slice(&bytes).ok()?;
    Some(manifest::from_entries(entries))
}

fn save_baseline(path: &Path, m: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entries: Vec<&FileEntry> = m.values().collect();
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(&entries)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn ask<S: Read + Write>(ch: &mut Channel<S>, request: &Request) -> Result<Response> {
    ch.send_json(request)?;
    match ch.recv_json()? {
        Response::Error { message } => Err(Error::Remote(message)),
        other => Ok(other),
    }
}

pub fn sync<S: Read + Write>(ch: &mut Channel<S>, req: &SaveSyncRequest) -> Result<Outcome> {
    let spec = match ask(ch, &Request::SaveSpec { game: req.game.clone() })? {
        Response::SaveSpec { spec } => spec,
        other => return Err(protocol(format!("expected a save spec, got {other:?}"))),
    };
    let Some(spec) = spec else { return Ok(Outcome::NoSpec) };
    let baseline = load_baseline(req.baseline_path);
    let here = manifest::build(saves::collect(&spec, req.roots), baseline.as_ref().unwrap_or(&Manifest::new()));
    let there = match ask(ch, &Request::SaveManifest { game: req.game.clone() })? {
        Response::Manifest { files } => manifest::from_entries(files.into_iter().filter(|f| spec.matches(&f.name)).collect()),
        other => return Err(protocol(format!("expected a manifest, got {other:?}"))),
    };
    let (plan, archive) = match (decide(&here, &there, baseline.as_ref()), req.choice) {
        (Plan::Conflict { .. }, Some(side)) => (copy_from(side, &here, &there), true),
        (plan, _) => (plan, false),
    };
    let mut base = baseline.unwrap_or_default();
    let result = match plan {
        Plan::Nothing => {
            base = here.clone();
            Ok(Outcome::UpToDate { files: here.len() })
        }
        Plan::Conflict { here, there } => return Ok(Outcome::Conflict { here, there }),
        Plan::Copy { from: Side::There, changed, removed } => pull(ch, req, &spec, &here, archive, &changed, &removed, &mut base)
            .map(|bytes| Outcome::Copied { from: Side::There, files: changed.len(), removed: removed.len(), bytes }),
        Plan::Copy { from: Side::Here, changed, removed } => push(ch, req, archive, &changed, &removed, &mut base)
            .map(|bytes| Outcome::Copied { from: Side::Here, files: changed.len(), removed: removed.len(), bytes }),
    };
    if result.is_ok() {
        // A finished copy leaves both sides equal to the winner.
        base = match &result {
            Ok(Outcome::Copied { from: Side::There, .. }) => there,
            Ok(Outcome::Copied { from: Side::Here, .. }) => here,
            _ => base,
        };
    }
    save_baseline(req.baseline_path, &base)?;
    result
}

#[allow(clippy::too_many_arguments)]
fn pull<S: Read + Write>(
    ch: &mut Channel<S>,
    req: &SaveSyncRequest,
    spec: &SaveSpec,
    here: &Manifest,
    archive: bool,
    changed: &[FileEntry],
    removed: &[String],
    base: &mut Manifest,
) -> Result<u64> {
    if archive {
        archive_files(here.values().filter_map(|e| Some((e.name.clone(), saves::local_path(&e.name, req.roots)?))), req.archive_dir)?;
    }
    let mut bytes = 0;
    for entry in changed {
        ch.send_json(&Request::FileGet { game: req.game.clone(), name: entry.name.clone() })?;
        let file = match ch.recv_json()? {
            Response::File { file } => file,
            Response::Error { message } => return Err(Error::Remote(message)),
            other => return Err(protocol(format!("expected a file, got {other:?}"))),
        };
        if manifest::key(&file.name) != manifest::key(&entry.name) || !spec.matches(&file.name) {
            return Err(protocol(format!("the computer sent {} for {}", file.name, entry.name)));
        }
        let target = saves::local_path(&file.name, req.roots).ok_or_else(|| protocol(format!("no place for {}", file.name)))?;
        receive_file(ch, &file, &target)?;
        bytes += file.size;
        base.insert(manifest::key(&file.name), file);
    }
    for name in removed {
        if !spec.matches(name) {
            continue;
        }
        if let Some(path) = saves::local_path(name, req.roots) {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        base.remove(&manifest::key(name));
    }
    Ok(bytes)
}

fn push<S: Read + Write>(
    ch: &mut Channel<S>,
    req: &SaveSyncRequest,
    archive: bool,
    changed: &[FileEntry],
    removed: &[String],
    base: &mut Manifest,
) -> Result<u64> {
    match ask(ch, &Request::SaveApply { game: req.game.clone(), remove: removed.to_vec(), archive })? {
        Response::Ok => {}
        other => return Err(protocol(format!("expected ok, got {other:?}"))),
    }
    for name in removed {
        base.remove(&manifest::key(name));
    }
    let mut bytes = 0;
    for entry in changed {
        let path = saves::local_path(&entry.name, req.roots).ok_or_else(|| protocol(format!("no place for {}", entry.name)))?;
        let file = entry_for(&entry.name, &path)?;
        ch.send_json(&Request::FilePut { game: req.game.clone(), file: file.clone() })?;
        send_contents(ch, &path, file.size)?;
        match ch.recv_json()? {
            Response::Ok => {}
            Response::Error { message } => return Err(Error::Remote(message)),
            other => return Err(protocol(format!("expected ok, got {other:?}"))),
        }
        bytes += file.size;
        base.insert(manifest::key(&file.name), file);
    }
    Ok(bytes)
}

/// A file's entry read now, for a header that must state its digest.
pub fn entry_for(name: &str, path: &Path) -> Result<FileEntry> {
    Ok(FileEntry {
        name: name.to_string(),
        size: path.metadata()?.len(),
        mtime_ms: manifest::mtime_ms(path)?,
        sha256: manifest::hash_file(path)?,
    })
}

/// Sends [`size`] bytes of [`path`] as raw messages after their header.
pub fn send_contents<S: Read + Write>(ch: &mut Channel<S>, path: &Path, size: u64) -> Result<()> {
    let mut file = File::open(path)?;
    let mut left = size;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let want = (left as usize).min(CHUNK);
        file.read_exact(&mut buf[..want])?;
        ch.send(&buf[..want])?;
        left -= want as u64;
    }
    Ok(())
}

/// Receives [`file`]'s contents and puts them at [`target`]: written beside
/// it, checked against the digest, then renamed over it with its time.
pub fn receive_file<S: Read + Write>(ch: &mut Channel<S>, file: &FileEntry, target: &Path) -> Result<()> {
    let parent = target.parent().ok_or_else(|| protocol("a file with no folder"))?;
    fs::create_dir_all(parent)?;
    let file_name = target.file_name().and_then(|n| n.to_str()).ok_or_else(|| protocol("a file with no name"))?;
    let part = parent.join(format!(".{file_name}{PART_SUFFIX}"));
    let mut out = File::create(&part)?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let outcome = (|| -> Result<()> {
        while received < file.size {
            let chunk = ch.recv()?;
            if chunk.is_empty() || received + chunk.len() as u64 > file.size {
                return Err(protocol("a file's contents did not match its size"));
            }
            hasher.update(&chunk);
            out.write_all(&chunk)?;
            received += chunk.len() as u64;
        }
        out.flush()?;
        if crate::hex::encode(&hasher.finalize()) != file.sha256 {
            return Err(protocol(format!("{} arrived damaged", file.name)));
        }
        out.set_modified(manifest::system_time(file.mtime_ms))?;
        Ok(())
    })();
    drop(out);
    match outcome {
        Ok(()) => {
            fs::rename(&part, target)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

/// UTC as `20261008T224500Z`, for archive folder names.
pub fn utc_stamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn is_stamp(name: &str) -> bool {
    name.len() == 16 && name.ends_with('Z') && name.as_bytes()[8] == b'T'
}

/// Copies [`files`] (canonical name, path) into a new folder under
/// [`archive_dir`], after removing the one before: the owner ruled out
/// versioning, so only the latest loser is kept.
pub fn archive_files(files: impl Iterator<Item = (String, PathBuf)>, archive_dir: &Path) -> Result<()> {
    let files: Vec<(String, PathBuf)> = files.filter(|(_, p)| p.is_file()).collect();
    if files.is_empty() {
        return Ok(());
    }
    if let Ok(read) = fs::read_dir(archive_dir) {
        for entry in read.flatten() {
            let name = entry.file_name();
            if name.to_str().is_some_and(is_stamp) && entry.file_type().is_ok_and(|t| t.is_dir()) {
                fs::remove_dir_all(archive_dir.join(name))?;
            }
        }
    }
    let dest = archive_dir.join(utc_stamp(SystemTime::now()));
    for (name, path) in files {
        let Some((token, parts)) = saves::parse_name(&name) else { continue };
        let mut target = dest.join(token.trim_matches(['<', '>']));
        for part in parts {
            target.push(part);
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&path, &target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(files: &[(&str, &str)]) -> Manifest {
        manifest::from_entries(
            files.iter().map(|(n, h)| FileEntry { name: n.to_string(), size: 1, mtime_ms: 1, sha256: h.to_string() }).collect(),
        )
    }

    #[test]
    fn one_side_changed_wins() {
        let base = m(&[("<base>/a", "1")]);
        let here = m(&[("<base>/a", "2")]);
        let there = base.clone();
        assert!(matches!(decide(&here, &there, Some(&base)), Plan::Copy { from: Side::Here, .. }));
        assert!(matches!(decide(&there, &here, Some(&base)), Plan::Copy { from: Side::There, .. }));
    }

    #[test]
    fn deletions_travel() {
        let base = m(&[("<base>/a", "1"), ("<base>/b", "1")]);
        let here = m(&[("<base>/a", "1")]);
        match decide(&here, &base, Some(&base)) {
            Plan::Copy { from: Side::Here, changed, removed } => {
                assert!(changed.is_empty());
                assert_eq!(removed, vec!["<base>/b".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn both_changed_or_first_sync_with_differences_is_a_conflict() {
        let base = m(&[("<base>/a", "1")]);
        assert!(matches!(decide(&m(&[("<base>/a", "2")]), &m(&[("<base>/a", "3")]), Some(&base)), Plan::Conflict { .. }));
        assert!(matches!(decide(&m(&[("<base>/a", "2")]), &m(&[("<base>/a", "3")]), None), Plan::Conflict { .. }));
        assert!(matches!(decide(&Manifest::new(), &m(&[("<base>/a", "3")]), None), Plan::Copy { from: Side::There, .. }));
        assert_eq!(decide(&base, &base, None), Plan::Nothing);
    }

    #[test]
    fn stamps_are_utc() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_791_499_500);
        assert_eq!(utc_stamp(t), "20261008T224500Z");
        assert!(is_stamp(&utc_stamp(t)));
    }
}
