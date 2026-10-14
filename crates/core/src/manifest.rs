//! A save set's state: every file's canonical name, size, modification time
//! and SHA-256. Files whose size and time match an earlier manifest keep its
//! digest instead of being read again, the same saving droidtop's Steam
//! Cloud sync makes.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub sha256: String,
}

/// Files by their name compared without case.
pub type Manifest = BTreeMap<String, FileEntry>;

pub fn key(name: &str) -> String {
    name.to_lowercase()
}

pub fn from_entries(entries: Vec<FileEntry>) -> Manifest {
    entries.into_iter().map(|e| (key(&e.name), e)).collect()
}

pub fn mtime_ms(path: &Path) -> io::Result<i64> {
    let modified = path.metadata()?.modified()?;
    Ok(match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    })
}

pub fn system_time(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms as u64)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}

pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::hex::encode(&hasher.finalize()))
}

/// The manifest of [`files`] (canonical name, path), reusing [`known`]'s
/// digests for files whose size and time did not change. A file that cannot
/// be read is left out, as if it were not there.
pub fn build(files: Vec<(String, PathBuf)>, known: &Manifest) -> Manifest {
    let mut out = Manifest::new();
    for (name, path) in files {
        let Ok(meta) = path.metadata() else { continue };
        let Ok(mtime) = mtime_ms(&path) else { continue };
        let size = meta.len();
        let sha256 = match known.get(&key(&name)) {
            Some(k) if k.size == size && k.mtime_ms == mtime => k.sha256.clone(),
            _ => match hash_file(&path) {
                Ok(h) => h,
                Err(_) => continue,
            },
        };
        out.insert(key(&name), FileEntry { name, size, mtime_ms: mtime, sha256 });
    }
    out
}

/// Whether two manifests hold the same files with the same contents.
pub fn same(a: &Manifest, b: &Manifest) -> bool {
    a.len() == b.len() && a.iter().all(|(k, e)| b.get(k).is_some_and(|o| o.sha256 == e.sha256))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_are_reused_only_when_size_and_time_match() {
        let dir = std::env::temp_dir().join(format!("dta-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.sav");
        std::fs::write(&path, b"one").unwrap();
        let files = vec![("<base>/a.sav".to_string(), path.clone())];
        let first = build(files.clone(), &Manifest::new());
        let entry = first.get("<base>/a.sav").unwrap();
        assert_eq!(entry.size, 3);
        let mut fake = first.clone();
        fake.get_mut("<base>/a.sav").unwrap().sha256 = "cached".into();
        assert_eq!(build(files.clone(), &fake).get("<base>/a.sav").unwrap().sha256, "cached");
        fake.get_mut("<base>/a.sav").unwrap().mtime_ms -= 5000;
        assert_eq!(build(files, &fake).get("<base>/a.sav").unwrap().sha256, entry.sha256);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
