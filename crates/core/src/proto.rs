//! The messages on the channel (docs/DESIGN.md section 5). The handheld
//! asks, the computer answers, one reply per request. File contents follow
//! the JSON header that announces them as raw messages of at most
//! [`CHUNK`] bytes.

use serde::{Deserialize, Serialize};

use crate::context::{RecordChange, Records};
use crate::library::Change;
use crate::manifest::FileEntry;
use crate::saves::SaveSpec;

/// The largest piece of a file's contents in one message.
pub const CHUNK: usize = 1024 * 1024;

/// A game as the handheld names it: its key (`steam:440`, `title:...`) and
/// its title, for the computer to find it by name when it has no store key.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameRef {
    pub key: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Request {
    Hello { name: String, version: u32, features: Vec<String> },
    LibraryPull { since: u64 },
    LibraryPush { changes: Vec<Change> },
    SaveSpec { game: GameRef },
    SaveManifest { game: GameRef },
    FileGet { game: GameRef, name: String },
    FilePut { game: GameRef, file: FileEntry },
    SaveApply { game: GameRef, remove: Vec<String>, archive: bool },
    ContextPull { context: String },
    ContextPush { context: String, changes: Vec<RecordChange> },
    Bye,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Response {
    Hello { name: String, version: u32, features: Vec<String> },
    Ok,
    Error { message: String },
    LibraryChanges { changes: Vec<Change>, cursor: u64 },
    SaveSpec { spec: Option<SaveSpec> },
    Manifest { files: Vec<FileEntry> },
    File { file: FileEntry },
    Context { records: Records },
    ContextApplied { deferred: bool, message: Option<String> },
}
