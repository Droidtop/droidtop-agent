//! Copying a game's folder between devices (docs/DESIGN.md section 7,
//! "Game updates"; Droidtop/tracker#469 part 3): the owner's "If I install an
//! update to a game on my desktop, I should be able to sync it to the
//! handheld and version-manage it automatically".
//!
//! A version is a folder (droidtop's SPEC 7m), so a copy is a new folder
//! beside the game's others, named for its version, never a change made in
//! place. The previous version stays playable, and rolling back is choosing
//! it. The copy is made in `<name>.dtpart/` and renamed into place only when
//! every file arrived whole. A copy that stops is picked up where it
//! stopped: a file already the right size and time is kept, and a shorter
//! one is continued from where it ends.
//!
//! On the wire: a manifest of the folder (relative paths, sizes, times), then
//! one file at a time, in [`CHUNK`] pieces from an offset, followed by the
//! whole file's SHA-256, which the receiver checks against its own.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::manifest;
use crate::proto::{GameRef, Request, Response, CHUNK};
use crate::{error::protocol, hex, Error, Result};

/// The suffix of a folder a copy is still being made in.
pub const PART: &str = ".dtpart";

/// One file of a game's folder.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GameFile {
    /// Relative to the game's folder, with forward slashes.
    pub path: String,
    pub size: u64,
    pub mtime_ms: i64,
}

/// A game's folder as the device that has it describes it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GameFolder {
    /// The folder's own name on that device.
    pub name: String,
    /// The version it has, as that device states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub files: Vec<GameFile>,
}

impl GameFolder {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Whether [`path`] is a plain relative path inside a folder: no root, no
/// drive, no `..`, no empty part.
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
}

/// A folder name a device may make: no separators, not `.` or `..`, and not
/// one that hides itself.
pub fn safe_folder_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', '\0', ':']) && name.len() <= 200
}

/// Lists [`root`]'s files (symbolic links are left out: a copy carries a
/// game's own files, not what they point at).
pub fn describe(root: &Path, version: Option<String>) -> Result<GameFolder> {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| protocol("a game folder with no name"))?;
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(GameFolder { name, version, files })
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<GameFile>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(root, &path, out)?;
        } else if kind.is_file() {
            let rel = path.strip_prefix(root).map_err(|_| protocol("a file outside its folder"))?;
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            let meta = entry.metadata()?;
            out.push(GameFile { path: rel, size: meta.len(), mtime_ms: manifest::mtime_ms(&path)? });
        }
    }
    Ok(())
}

/// How far a copy has come, for the person to see.
pub trait Progress {
    fn progress(&mut self, done: u64, total: u64);
}

impl<F: FnMut(u64, u64)> Progress for F {
    fn progress(&mut self, done: u64, total: u64) {
        self(done, total)
    }
}

/// Sends [`root`]'s file [`path`] from [`offset`]: the header, the rest of the
/// file in pieces, then the whole file's digest.
pub fn send_file<S: Read + Write>(ch: &mut Channel<S>, root: &Path, path: &str, offset: u64) -> Result<()> {
    if !safe_relative(path) {
        return Err(protocol(format!("{path} is not a path inside the game's folder")));
    }
    let full = root.join(path);
    let size = full.metadata()?.len();
    let mut file = File::open(&full)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    // The part the other side has is hashed here too, so the digest covers the whole file.
    let mut left = offset.min(size);
    while left > 0 {
        let n = file.read(&mut buf[..(left as usize).min(CHUNK)])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    ch.send_json(&Response::GameFile { path: path.to_string(), size, offset: offset.min(size) })?;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        ch.send(&buf[..n])?;
    }
    ch.send(&[])?;
    ch.send_json(&Response::Digest { sha256: hex::encode(&hasher.finalize()) })
}

/// Receives one file into [`target`], continuing from what it already
/// holds, and checks the whole file against the digest that follows.
fn receive_into<S: Read + Write>(ch: &mut Channel<S>, target: &Path, expected: &GameFile, progress: &mut dyn FnMut(u64)) -> Result<()> {
    let (size, offset) = match ch.recv_json()? {
        Response::GameFile { path, size, offset } if path == expected.path => (size, offset),
        Response::Error { message } => return Err(Error::Remote(message)),
        other => return Err(protocol(format!("expected {}, got {other:?}", expected.path))),
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).truncate(false).write(true).read(true).open(target)?;
    file.set_len(offset)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut got = offset;
    loop {
        let chunk = ch.recv()?;
        if chunk.is_empty() {
            break;
        }
        got += chunk.len() as u64;
        if got > size {
            return Err(protocol(format!("{} was longer than it said", expected.path)));
        }
        file.write_all(&chunk)?;
        progress(chunk.len() as u64);
    }
    file.flush()?;
    let sha256 = match ch.recv_json()? {
        Response::Digest { sha256 } => sha256,
        other => return Err(protocol(format!("expected a digest, got {other:?}"))),
    };
    if got != size || manifest::hash_file(target)? != sha256 {
        // Started again from nothing next time.
        let _ = fs::remove_file(target);
        return Err(protocol(format!("{} arrived damaged", expected.path)));
    }
    drop(file);
    let f = OpenOptions::new().write(true).open(target)?;
    f.set_modified(manifest::system_time(expected.mtime_ms))?;
    Ok(())
}

/// Whether a file already in a part folder is the one [`want`] names: the
/// same size and time.
fn already_here(path: &Path, want: &GameFile) -> bool {
    path.metadata().is_ok_and(|m| m.len() == want.size) && manifest::mtime_ms(path).is_ok_and(|t| t == want.mtime_ms)
}

/// What a copy did.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// The folder the copy is now in.
    pub folder: PathBuf,
    pub files: usize,
    /// What travelled this time (a resumed copy carries less than the game's size).
    pub bytes: u64,
}

/// Copies the computer's folder of [`game`] here, into a new folder named
/// [`folder_name`] inside [`parent`].
pub fn pull<S: Read + Write>(
    ch: &mut Channel<S>,
    game: &GameRef,
    parent: &Path,
    folder_name: &str,
    progress: &mut dyn Progress,
) -> Result<Copied> {
    if !safe_folder_name(folder_name) {
        return Err(protocol(format!("{folder_name} is not a folder name")));
    }
    let target = parent.join(folder_name);
    if target.exists() {
        return Err(protocol(format!("{} is already there", target.display())));
    }
    ch.send_json(&Request::GameFiles { game: game.clone() })?;
    let folder = match ch.recv_json()? {
        Response::GameFiles { folder } => folder,
        Response::Error { message } => return Err(Error::Remote(message)),
        other => return Err(protocol(format!("expected a game's files, got {other:?}"))),
    };
    if let Some(bad) = folder.files.iter().find(|f| !safe_relative(&f.path)) {
        return Err(protocol(format!("the computer named a file outside the game's folder: {}", bad.path)));
    }
    let part = parent.join(format!("{folder_name}{PART}"));
    fs::create_dir_all(&part)?;
    let total = folder.bytes();
    let mut done: u64 = 0;
    let mut moved: u64 = 0;
    for f in &folder.files {
        let path = part.join(&f.path);
        if already_here(&path, f) {
            done += f.size;
            progress.progress(done, total);
            continue;
        }
        let offset = path.metadata().map(|m| m.len()).unwrap_or(0).min(f.size);
        done += offset;
        ch.send_json(&Request::GameFileGet { game: game.clone(), path: f.path.clone(), offset })?;
        let mut step = |n: u64| {
            done += n;
            moved += n;
            progress.progress(done, total);
        };
        receive_into(ch, &path, f, &mut step)?;
    }
    fs::rename(&part, &target)?;
    Ok(Copied { folder: target, files: folder.files.len(), bytes: moved })
}

/// Copies this device's folder [`source`] of [`game`] to the computer, which
/// puts it in a new folder named [`folder_name`] among its game folders.
pub fn push<S: Read + Write>(
    ch: &mut Channel<S>,
    game: &GameRef,
    source: &Path,
    folder_name: &str,
    version: Option<String>,
    progress: &mut dyn Progress,
) -> Result<Copied> {
    let mut folder = describe(source, version)?;
    folder.name = folder_name.to_string();
    if !safe_folder_name(folder_name) {
        return Err(protocol(format!("{folder_name} is not a folder name")));
    }
    // The computer says how much of each file it already holds.
    ch.send_json(&Request::GamePutStart { game: game.clone(), folder: folder.clone() })?;
    let have = match ch.recv_json()? {
        Response::GamePutHave { have } => have,
        Response::Error { message } => return Err(Error::Remote(message)),
        other => return Err(protocol(format!("expected what the computer has, got {other:?}"))),
    };
    let total = folder.bytes();
    let mut done: u64 = 0;
    let mut moved: u64 = 0;
    for (f, held) in folder.files.iter().zip(have) {
        if held >= f.size && held != u64::MAX {
            done += f.size;
            progress.progress(done, total);
            continue;
        }
        let offset = if held == u64::MAX { 0 } else { held };
        ch.send_json(&Request::GameFilePut { game: game.clone(), path: f.path.clone() })?;
        let before = done;
        send_file(ch, source, &f.path, offset)?;
        match ch.recv_json()? {
            Response::Ok => {}
            Response::Error { message } => return Err(Error::Remote(message)),
            other => return Err(protocol(format!("expected ok, got {other:?}"))),
        }
        done = before + f.size;
        moved += f.size - offset;
        progress.progress(done, total);
    }
    let placed = match ask(ch, &Request::GamePutFinish { game: game.clone() })? {
        Response::GamePlaced { folder } => folder,
        other => return Err(protocol(format!("expected the folder the computer made, got {other:?}"))),
    };
    Ok(Copied { folder: PathBuf::from(placed), files: folder.files.len(), bytes: moved })
}

fn ask<S: Read + Write>(ch: &mut Channel<S>, request: &Request) -> Result<Response> {
    ch.send_json(request)?;
    match ch.recv_json()? {
        Response::Error { message } => Err(Error::Remote(message)),
        other => Ok(other),
    }
}

/// The computer's side of a push: where the folder is being made, and what it
/// should hold.
pub struct Incoming {
    pub part: PathBuf,
    pub target: PathBuf,
    pub folder: GameFolder,
}

impl Incoming {
    /// Starts (or continues) receiving [`folder`] into [`parent`]. Returns,
    /// per file, how much is already here: its size when it is whole,
    /// `u64::MAX` when it must start over, else the bytes it holds.
    pub fn start(parent: &Path, folder: GameFolder) -> Result<(Incoming, Vec<u64>)> {
        if !safe_folder_name(&folder.name) {
            return Err(protocol(format!("{} is not a folder name", folder.name)));
        }
        if let Some(bad) = folder.files.iter().find(|f| !safe_relative(&f.path)) {
            return Err(protocol(format!("{} is not a path inside a game's folder", bad.path)));
        }
        let target = parent.join(&folder.name);
        if target.exists() {
            return Err(protocol(format!("{} is already there", target.display())));
        }
        let part = parent.join(format!("{}{PART}", folder.name));
        fs::create_dir_all(&part)?;
        let have = folder
            .files
            .iter()
            .map(|f| {
                let p = part.join(&f.path);
                if already_here(&p, f) {
                    f.size
                } else {
                    p.metadata().map(|m| m.len().min(f.size.saturating_sub(1))).unwrap_or(u64::MAX)
                }
            })
            .collect();
        Ok((Incoming { part, target, folder }, have))
    }

    /// Receives one file the device announced.
    pub fn receive<S: Read + Write>(&self, ch: &mut Channel<S>, path: &str) -> Result<()> {
        let want = self.folder.files.iter().find(|f| f.path == path).ok_or_else(|| protocol(format!("{path} is not in the folder")))?;
        receive_into(ch, &self.part.join(path), want, &mut |_| {})
    }

    /// Puts the folder in place once every file is whole.
    pub fn finish(self) -> Result<PathBuf> {
        if let Some(missing) = self.folder.files.iter().find(|f| !already_here(&self.part.join(&f.path), f)) {
            return Err(protocol(format!("{} has not arrived yet", missing.path)));
        }
        fs::rename(&self.part, &self.target)?;
        Ok(self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_relative_paths_and_names_pass() {
        assert!(safe_relative("game/data.pak"));
        assert!(!safe_relative("../etc/passwd"));
        assert!(!safe_relative("/etc/passwd"));
        assert!(!safe_relative("a/../b"));
        assert!(!safe_relative("C:\\x"));
        assert!(safe_folder_name("Testgame v1.1"));
        assert!(!safe_folder_name(".hidden"));
        assert!(!safe_folder_name("a/b"));
    }
}
