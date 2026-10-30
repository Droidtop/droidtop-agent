//! A syncable data context's copy (docs/DESIGN.md section 8): the one thing
//! of a plugin's data that travels, kept apart from the data the plugin
//! works from. A copy is a whole set of records, made current in one step;
//! nothing reads half of one and a write that stops changes nothing.
//!
//! On the computer it is `<adapters>/<context>/copy/current.json`. A new copy
//! is written beside it as `incoming-<n>.json`, flushed to disk and renamed
//! over `current.json`, so a reader sees the old copy or the new one. The
//! copy carries a generation that goes up by one with every copy made
//! current, so a writer built on an older copy is told so ([`Stale`]) and
//! merges again instead of overwriting what it never saw.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::context::Records;
use crate::{Error, Result};

/// A file that belongs to a copy (a context whose data is more than records,
/// such as Syncthing's index). Files travel by the piece transfer, checked by
/// digest, and are never merged; the type is here so adding them later
/// changes no message. Empty for a context of records alone.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CopyFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Copy {
    /// Goes up by one each time a copy is made current here; 0 is no copy yet.
    pub generation: u64,
    pub records: Records,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<CopyFile>,
}

/// The error text a writer built on an older copy is refused with
/// ([`Error::Protocol`]), so the other end can tell it from a failure.
pub const STALE: &str = "stale";

/// A lock left by a program that stopped is taken over after this long.
const LOCK_STALE: Duration = Duration::from_secs(60);

/// Whether [`id`] can name a context (and its folder under `adapters/`).
pub fn valid_context(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// One context's copy on this device.
pub struct CopyStore {
    dir: PathBuf,
}

impl CopyStore {
    /// The copy folder of [`context`] under [`adapters`] (the agent's
    /// `adapters/` folder). Leftovers of a write that stopped are removed.
    pub fn open(adapters: &Path, context: &str) -> Result<Self> {
        if !valid_context(context) {
            return Err(Error::Protocol(format!("{context:?} cannot name a context")));
        }
        let dir = adapters.join(context).join("copy");
        fs::create_dir_all(&dir)?;
        let store = Self { dir };
        store.clear_leftovers();
        Ok(store)
    }

    fn current_path(&self) -> PathBuf {
        self.dir.join("current.json")
    }

    fn clear_leftovers(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("incoming-") {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    /// The current copy; generation 0 with no records when none was made.
    pub fn current(&self) -> Result<Copy> {
        match fs::read(self.current_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Copy::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Makes a new copy current when [`base`] is the generation it was built
    /// on (the plugin's own publish passes the current one). Returns the new
    /// generation, or [`STALE`] when another copy became current first.
    pub fn replace(&self, base: u64, records: Records, files: Vec<CopyFile>) -> Result<u64> {
        let _lock = Lock::take(&self.dir)?;
        let current = self.current()?;
        if current.generation != base {
            return Err(Error::Protocol(STALE.into()));
        }
        let next = Copy { generation: current.generation + 1, records, files };
        let tmp = self.dir.join(format!("incoming-{}.json", std::process::id()));
        let mut file = File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(&next)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, self.current_path())?;
        sync_dir(&self.dir);
        Ok(next.generation)
    }

    /// The plugin published a new copy: it becomes current whatever the
    /// generation was (the plugin's data is the newest by definition).
    pub fn publish(&self, records: Records, files: Vec<CopyFile>) -> Result<u64> {
        loop {
            let base = self.current()?.generation;
            match self.replace(base, records.clone(), files.clone()) {
                Err(Error::Protocol(m)) if m == STALE => continue,
                other => return other,
            }
        }
    }
}

/// Makes the rename itself durable where the system lets a folder be flushed.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Keeps two writers (this program and `droidtop-agent contexts publish`)
/// from checking one generation and both writing.
struct Lock(PathBuf);

impl Lock {
    fn take(dir: &Path) -> Result<Self> {
        let path = dir.join("lock");
        for _ in 0..2 {
            match File::options().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let old = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > LOCK_STALE);
                    if !old {
                        return Err(Error::Protocol("another copy is being made current; try again".into()));
                    }
                    let _ = fs::remove_file(&path);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::Protocol("another copy is being made current; try again".into()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn records(v: &str) -> Records {
        [("100".to_string(), [("installed".to_string(), json!(v))].into_iter().collect())].into_iter().collect()
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dta-copy-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn only_a_plain_name_opens_a_copy() {
        let root = dir("names");
        assert!(CopyStore::open(&root, "../escape").is_err());
        assert!(CopyStore::open(&root, ".hidden").is_err());
        assert!(CopyStore::open(&root, "").is_err());
        assert!(CopyStore::open(&root, "f95checker").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_copy_is_made_current_in_one_step_and_the_generation_goes_up() {
        let root = dir("steps");
        let store = CopyStore::open(&root, "f95checker").unwrap();
        assert_eq!(store.current().unwrap(), Copy::default());
        assert_eq!(store.replace(0, records("0.1"), Vec::new()).unwrap(), 1);
        assert_eq!(store.replace(1, records("0.2"), Vec::new()).unwrap(), 2);
        let now = store.current().unwrap();
        assert_eq!((now.generation, now.records), (2, records("0.2")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_writer_built_on_an_older_copy_is_refused_and_nothing_changes() {
        let root = dir("stale");
        let store = CopyStore::open(&root, "f95checker").unwrap();
        store.replace(0, records("0.1"), Vec::new()).unwrap();
        store.replace(1, records("0.2"), Vec::new()).unwrap();
        let err = store.replace(1, records("0.9"), Vec::new()).unwrap_err();
        assert!(matches!(&err, Error::Protocol(m) if m == STALE), "{err:?}");
        assert_eq!(store.current().unwrap().records, records("0.2"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_write_that_stopped_changes_nothing_and_leaves_nothing_behind() {
        let root = dir("stopped");
        let store = CopyStore::open(&root, "f95checker").unwrap();
        store.replace(0, records("0.1"), Vec::new()).unwrap();
        // The program died after writing the new copy apart and before the rename.
        let leftover = root.join("f95checker/copy/incoming-1.json");
        fs::write(&leftover, b"{\"generation\": 2, \"records\": {").unwrap();
        // A reader sees the old copy whole, and the next open clears the leftover.
        let reopened = CopyStore::open(&root, "f95checker").unwrap();
        let now = reopened.current().unwrap();
        assert_eq!((now.generation, now.records), (1, records("0.1")));
        assert!(!leftover.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_publish_becomes_current_whatever_the_generation() {
        let root = dir("publish");
        let store = CopyStore::open(&root, "f95checker").unwrap();
        store.replace(0, records("0.1"), Vec::new()).unwrap();
        assert_eq!(store.publish(records("0.5"), Vec::new()).unwrap(), 2);
        assert_eq!(store.current().unwrap().records, records("0.5"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_lock_a_program_left_behind_is_taken_over_when_old() {
        let root = dir("lock");
        let store = CopyStore::open(&root, "f95checker").unwrap();
        let lock = root.join("f95checker/copy/lock");
        fs::write(&lock, b"").unwrap();
        // Fresh: someone is writing, so this is refused.
        assert!(store.replace(0, records("0.1"), Vec::new()).is_err());
        // Old: its writer is gone.
        File::options().write(true).open(&lock).unwrap().set_modified(SystemTime::now() - Duration::from_secs(120)).unwrap();
        assert_eq!(store.replace(0, records("0.1"), Vec::new()).unwrap(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
