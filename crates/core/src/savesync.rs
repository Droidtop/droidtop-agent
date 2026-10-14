//! Syncing one game's saves (docs/DESIGN.md sections 6 and 9).
//!
//! Against the files as they were at the last sync (the baseline the
//! handheld keeps per computer and game), a side that alone changed wins and
//! its set is copied over, deletions included. When both changed, or a first
//! sync finds different files on each side, the newest copy wins (owner,
//! 2026-10-10: "newest copy wins isn't the worst idea. We keep the newest
//! copy from EACH device, and an archive if possible. We can also set a
//! primary desktop and primary device to prefer saves from"):
//! - a side the person prefers wins: the handheld's primary computer, or the
//!   computer's primary handheld; when both sides prefer each other, or
//!   neither, the set with the newer newest file wins;
//! - the other side's changed set is not lost: before it is overwritten, the
//!   device keeps it as its own copy ([`keep_copy`]), and the copy it had
//!   kept before goes to a short dated archive.
//!
//! The person can still pick a side ([`SaveSyncRequest::choice`]), to bring
//! back the other device's set.
//!
//! The baseline moves file by file, so a sync cut short is finished by the
//! next one instead of being read as a change on both sides.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::keys::{DeviceKey, PeerId};
use crate::mailbox::{self, Envelope};
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

/// Which side the person prefers saves from, when both changed.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Prefer {
    /// The computer names this handheld its primary handheld.
    #[serde(default)]
    pub here: bool,
    /// The handheld names this computer its primary computer.
    #[serde(default)]
    pub there: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    Nothing,
    /// [`keep`]: the side being overwritten had changed too, so it keeps
    /// its set as a copy first.
    Copy {
        from: Side,
        changed: Vec<FileEntry>,
        removed: Vec<String>,
        keep: bool,
    },
}

fn copy_from(side: Side, here: &Manifest, there: &Manifest, keep: bool) -> Plan {
    let (src, dst) = match side {
        Side::Here => (here, there),
        Side::There => (there, here),
    };
    let changed = src.iter().filter(|(k, e)| dst.get(*k).is_none_or(|d| d.sha256 != e.sha256)).map(|(_, e)| e.clone()).collect();
    let removed = dst.iter().filter(|(k, _)| !src.contains_key(*k)).map(|(_, e)| e.name.clone()).collect();
    Plan::Copy { from: side, changed, removed, keep }
}

/// The time of a set's newest file.
pub fn newest_ms(m: &Manifest) -> i64 {
    m.values().map(|e| e.mtime_ms).max().unwrap_or(0)
}

/// The side that wins when both changed: the preferred one, else the newest copy.
pub fn winner(here: &Manifest, there: &Manifest, prefer: Prefer) -> Side {
    match (prefer.here, prefer.there) {
        (true, false) => Side::Here,
        (false, true) => Side::There,
        _ if newest_ms(there) > newest_ms(here) => Side::There,
        _ => Side::Here,
    }
}

/// What to do, from both sides' files and the baseline.
pub fn decide(here: &Manifest, there: &Manifest, baseline: Option<&Manifest>, prefer: Prefer) -> Plan {
    if manifest::same(here, there) {
        return Plan::Nothing;
    }
    let both = || copy_from(winner(here, there, prefer), here, there, true);
    match baseline {
        None if here.is_empty() => copy_from(Side::There, here, there, false),
        None if there.is_empty() => copy_from(Side::Here, here, there, false),
        None => both(),
        Some(base) => match (!manifest::same(here, base), !manifest::same(there, base)) {
            (true, false) => copy_from(Side::Here, here, there, false),
            (false, true) => copy_from(Side::There, here, there, false),
            _ => both(),
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
        /// Both sides had changed: the other side's set was kept as that
        /// device's copy before it was overwritten.
        #[serde(default)]
        kept: bool,
    },
}

/// One sync, from the handheld's side.
pub struct SaveSyncRequest<'a> {
    pub game: GameRef,
    pub roots: &'a Roots,
    /// The baseline file for this computer and game.
    pub baseline_path: &'a Path,
    /// This game's archive here: the copies kept per device, and older ones.
    pub archive_dir: &'a Path,
    /// This device's name, for the copy it keeps of its own saves.
    pub name: &'a str,
    /// This computer is the handheld's primary computer.
    pub primary_computer: bool,
    /// The person picked a side (to bring back the other device's saves).
    pub choice: Option<Side>,
}

/// Where the computer's save locations for a game are kept beside its
/// baseline, so its saves can be left in the share when it is away.
pub fn spec_path(baseline: &Path) -> PathBuf {
    baseline.with_extension("spec.json")
}

/// The save set last left in the share for the computer, kept until a live
/// sync settles both sides again.
pub fn posted_path(baseline: &Path) -> PathBuf {
    baseline.with_extension("posted.json")
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
    // Kept for a save set left in the share while the computer is away; a
    // failure to keep it only means that path waits for the next live sync.
    if let Ok(bytes) = serde_json::to_vec(&spec) {
        if let Some(parent) = req.baseline_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(spec_path(req.baseline_path), bytes);
    }
    let baseline = load_baseline(req.baseline_path);
    let here = manifest::build(saves::collect(&spec, req.roots), baseline.as_ref().unwrap_or(&Manifest::new()));
    let (there, primary_handheld) = match ask(ch, &Request::SaveManifest { game: req.game.clone() })? {
        Response::Manifest { files, primary } => {
            (manifest::from_entries(files.into_iter().filter(|f| spec.matches(&f.name)).collect()), primary)
        }
        other => return Err(protocol(format!("expected a manifest, got {other:?}"))),
    };
    let prefer = Prefer { here: primary_handheld, there: req.primary_computer };
    let plan = match (decide(&here, &there, baseline.as_ref(), prefer), req.choice) {
        // The person's own pick replaces the rule; the side it overwrites keeps a copy.
        (Plan::Copy { from, .. }, Some(side)) if from != side => copy_from(side, &here, &there, true),
        (plan, _) => plan,
    };
    let mut base = baseline.unwrap_or_default();
    let result = match plan {
        Plan::Nothing => {
            base = here.clone();
            Ok(Outcome::UpToDate { files: here.len() })
        }
        Plan::Copy { from: Side::There, changed, removed, keep } => pull(ch, req, &spec, &here, keep, &changed, &removed, &mut base)
            .map(|bytes| Outcome::Copied { from: Side::There, files: changed.len(), removed: removed.len(), bytes, kept: keep }),
        Plan::Copy { from: Side::Here, changed, removed, keep } => push(ch, req, keep, &changed, &removed, &mut base)
            .map(|bytes| Outcome::Copied { from: Side::Here, files: changed.len(), removed: removed.len(), bytes, kept: keep }),
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
    if result.is_ok() {
        // Both sides are settled live, so a set left in the share earlier
        // is no longer what the computer is assumed to have.
        let _ = fs::remove_file(posted_path(req.baseline_path));
    }
    result
}

/// What leaving a game's saves in the share did.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Posted {
    /// No live sync with the computer has said where the game's saves are,
    /// or settled a baseline, yet.
    NotYet,
    /// The saves match what the computer has or was last sent.
    UpToDate {
        files: usize,
    },
    Posted {
        files: usize,
        bytes: u64,
    },
}

/// Leaves [`game`]'s saves in [`share`] for the computer [`to`] when they
/// changed since the computer last had them (docs/DESIGN.md section 10,
/// transport 3). The set states the files it was made against: the last
/// live baseline, or the set posted before it when that one has not been
/// settled live yet. When the computer's saves changed too, the newest copy
/// wins there, as in a live sync, and the other set is kept as a copy.
pub fn post_to_share(
    share: &Path,
    key: &DeviceKey,
    to: &PeerId,
    game: &GameRef,
    roots: &Roots,
    baseline_path: &Path,
    primary_computer: bool,
) -> Result<Posted> {
    let spec: Option<SaveSpec> = fs::read(spec_path(baseline_path)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let posted = posted_path(baseline_path);
    let (Some(spec), Some(base)) = (spec, load_baseline(&posted).or_else(|| load_baseline(baseline_path))) else {
        return Ok(Posted::NotYet);
    };
    let mut files = Vec::new();
    let mut contents = Vec::new();
    for (name, path) in saves::collect(&spec, roots) {
        let (Ok(bytes), Ok(mtime_ms)) = (fs::read(&path), manifest::mtime_ms(&path)) else { continue };
        files.push(FileEntry { name, size: bytes.len() as u64, mtime_ms, sha256: crate::hex::encode(&Sha256::digest(&bytes)) });
        contents.push(bytes);
    }
    let here = manifest::from_entries(files.clone());
    if manifest::same(&here, &base) {
        return Ok(Posted::UpToDate { files: here.len() });
    }
    let bytes = contents.iter().map(|c| c.len() as u64).sum();
    let envelope = Envelope::Saves { game: game.clone(), base: base.into_values().collect(), files, primary_computer };
    mailbox::post(share, key, to, &mailbox::pack(&envelope, &contents)?)?;
    save_baseline(&posted, &here)?;
    Ok(Posted::Posted { files: here.len(), bytes })
}

#[allow(clippy::too_many_arguments)]
fn pull<S: Read + Write>(
    ch: &mut Channel<S>,
    req: &SaveSyncRequest,
    spec: &SaveSpec,
    here: &Manifest,
    keep: bool,
    changed: &[FileEntry],
    removed: &[String],
    base: &mut Manifest,
) -> Result<u64> {
    if keep {
        keep_copy(here.values().filter_map(|e| Some((e.name.clone(), saves::local_path(&e.name, req.roots)?))), req.archive_dir, req.name)?;
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
    keep: bool,
    changed: &[FileEntry],
    removed: &[String],
    base: &mut Manifest,
) -> Result<u64> {
    // The computer keeps its own changed set as its copy before it is overwritten.
    match ask(ch, &Request::SaveApply { game: req.game.clone(), remove: removed.to_vec(), archive: keep })? {
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

/// How many older copies a game's dated archive keeps.
pub const ARCHIVED: usize = 5;

/// The folder of [`device`]'s kept copy in a game's archive.
pub fn copy_dir(archive_dir: &Path, device: &str) -> PathBuf {
    let name: String = device.chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).collect();
    let name = name.trim().trim_matches('.');
    archive_dir.join("copies").join(if name.is_empty() { "device" } else { name })
}

/// Keeps [`files`] (canonical name, path) as [`device`]'s copy of the game's
/// saves: the newest copy from each device is kept under `copies/<device>/`.
/// The copy this replaces moves into the dated archive, which keeps the
/// [`ARCHIVED`] most recent.
pub fn keep_copy(files: impl Iterator<Item = (String, PathBuf)>, archive_dir: &Path, device: &str) -> Result<()> {
    let files: Vec<(String, PathBuf)> = files.filter(|(_, p)| p.is_file()).collect();
    if files.is_empty() {
        return Ok(());
    }
    let dest = copy_dir(archive_dir, device);
    if dest.is_dir() {
        let stamp = utc_stamp(SystemTime::now());
        let mut older = archive_dir.join(&stamp);
        let mut n = 1;
        while older.exists() {
            older = archive_dir.join(format!("{stamp}-{n}"));
            n += 1;
        }
        fs::rename(&dest, &older)?;
        let _ = fs::write(older.join("device.txt"), device);
        prune(archive_dir)?;
    }
    write_set(files, &dest)
}

/// The dated archive, oldest first.
pub fn archived(archive_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(archive_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().and_then(|n| n.get(..16)).is_some_and(is_stamp) && e.path().is_dir())
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn prune(archive_dir: &Path) -> Result<()> {
    let all = archived(archive_dir);
    for old in &all[..all.len().saturating_sub(ARCHIVED)] {
        fs::remove_dir_all(old)?;
    }
    Ok(())
}

/// Copies a save set into [`dest`], laid out by root token.
fn write_set(files: Vec<(String, PathBuf)>, dest: &Path) -> Result<()> {
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

    fn at(files: &[(&str, &str, i64)]) -> Manifest {
        manifest::from_entries(
            files.iter().map(|(n, h, t)| FileEntry { name: n.to_string(), size: 1, mtime_ms: *t, sha256: h.to_string() }).collect(),
        )
    }

    fn m(files: &[(&str, &str)]) -> Manifest {
        at(&files.iter().map(|(n, h)| (*n, *h, 1)).collect::<Vec<_>>())
    }

    const NONE: Prefer = Prefer { here: false, there: false };

    #[test]
    fn one_side_changed_wins() {
        let base = m(&[("<base>/a", "1")]);
        let here = m(&[("<base>/a", "2")]);
        let there = base.clone();
        assert!(matches!(decide(&here, &there, Some(&base), NONE), Plan::Copy { from: Side::Here, keep: false, .. }));
        assert!(matches!(decide(&there, &here, Some(&base), NONE), Plan::Copy { from: Side::There, keep: false, .. }));
    }

    #[test]
    fn deletions_travel() {
        let base = m(&[("<base>/a", "1"), ("<base>/b", "1")]);
        let here = m(&[("<base>/a", "1")]);
        match decide(&here, &base, Some(&base), NONE) {
            Plan::Copy { from: Side::Here, changed, removed, .. } => {
                assert!(changed.is_empty());
                assert_eq!(removed, vec!["<base>/b".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn when_both_changed_the_newest_copy_wins_and_the_other_is_kept() {
        let base = at(&[("<base>/a", "1", 10)]);
        let older = at(&[("<base>/a", "2", 20)]);
        let newer = at(&[("<base>/a", "3", 30)]);
        assert!(matches!(decide(&older, &newer, Some(&base), NONE), Plan::Copy { from: Side::There, keep: true, .. }));
        assert!(matches!(decide(&newer, &older, Some(&base), NONE), Plan::Copy { from: Side::Here, keep: true, .. }));
        // A first sync with different files on each side follows the same rule.
        assert!(matches!(decide(&older, &newer, None, NONE), Plan::Copy { from: Side::There, keep: true, .. }));
        assert!(matches!(decide(&Manifest::new(), &newer, None, NONE), Plan::Copy { from: Side::There, keep: false, .. }));
        assert_eq!(decide(&base, &base, None, NONE), Plan::Nothing);
    }

    #[test]
    fn a_preferred_side_beats_a_newer_one() {
        let base = at(&[("<base>/a", "1", 10)]);
        let (older, newer) = (at(&[("<base>/a", "2", 20)]), at(&[("<base>/a", "3", 30)]));
        let primary_computer = Prefer { here: false, there: true };
        let primary_handheld = Prefer { here: true, there: false };
        assert!(matches!(decide(&newer, &older, Some(&base), primary_computer), Plan::Copy { from: Side::There, keep: true, .. }));
        assert!(matches!(decide(&older, &newer, Some(&base), primary_handheld), Plan::Copy { from: Side::Here, keep: true, .. }));
        // Each preferring the other settles nothing: the newest wins.
        let both = Prefer { here: true, there: true };
        assert!(matches!(decide(&older, &newer, Some(&base), both), Plan::Copy { from: Side::There, .. }));
    }

    #[test]
    fn each_device_keeps_its_newest_copy_and_a_short_archive() {
        let dir = std::env::temp_dir().join(format!("dta-keep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src.sav");
        let archive = dir.join("archive");
        for round in 0..(ARCHIVED + 3) {
            fs::create_dir_all(&dir).unwrap();
            fs::write(&src, format!("round {round}")).unwrap();
            keep_copy(std::iter::once(("<base>/slot.sav".to_string(), src.clone())), &archive, "Retroid Pocket 5").unwrap();
            keep_copy(std::iter::once(("<base>/slot.sav".to_string(), src.clone())), &archive, "DESKTOP/PC").unwrap();
        }
        let last = format!("round {}", ARCHIVED + 2);
        assert_eq!(fs::read_to_string(copy_dir(&archive, "Retroid Pocket 5").join("base/slot.sav")).unwrap(), last);
        assert_eq!(fs::read_to_string(archive.join("copies/DESKTOP_PC/base/slot.sav")).unwrap(), last);
        assert_eq!(archived(&archive).len(), ARCHIVED);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stamps_are_utc() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_791_499_500);
        assert_eq!(utc_stamp(t), "20261008T224500Z");
        assert!(is_stamp(&utc_stamp(t)));
    }
}
