//! The messages on the channel (docs/DESIGN.md section 5). The handheld
//! asks, the computer answers, one reply per request. File contents follow
//! the JSON header that announces them as raw messages of at most
//! [`CHUNK`] bytes.

use serde::{Deserialize, Serialize};

use crate::context::{AdapterOffer, RecordChange, Records};
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
    /// [`disco`] is the device's global discovery ID (crate::rendezvous).
    Hello {
        name: String,
        version: u32,
        features: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disco: Option<String>,
    },
    LibraryPull {
        since: u64,
    },
    LibraryPush {
        changes: Vec<Change>,
    },
    SaveSpec {
        game: GameRef,
    },
    SaveManifest {
        game: GameRef,
    },
    FileGet {
        game: GameRef,
        name: String,
    },
    FilePut {
        game: GameRef,
        file: FileEntry,
    },
    SaveApply {
        game: GameRef,
        remove: Vec<String>,
        archive: bool,
    },
    /// A context's records on the computer; [`adapter`] is the program the
    /// plugin offers for it, for a computer that has none yet.
    ContextPull {
        context: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adapter: Option<AdapterOffer>,
    },
    ContextPush {
        context: String,
        changes: Vec<RecordChange>,
    },
    Bye,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Response {
    /// The computer's name and what it supports, and where its WireGuard
    /// answers from outside the LAN (`wg:<ip>:<port>`), for the handheld
    /// to keep for when it is away.
    Hello {
        name: String,
        version: u32,
        features: Vec<String>,
        #[serde(default)]
        endpoints: Vec<String>,
        /// The computer's global discovery ID, to find it away from the LAN.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disco: Option<String>,
        /// The handheld reached the computer at an identity it is moving
        /// away from: the new one, signed by both (crate::moved).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        moved_to: Option<crate::moved::Moved>,
    },
    Ok,
    Error {
        message: String,
    },
    LibraryChanges {
        changes: Vec<Change>,
        cursor: u64,
    },
    SaveSpec {
        spec: Option<SaveSpec>,
    },
    /// [`primary`]: the computer names this handheld its primary handheld,
    /// whose saves it prefers when both changed.
    Manifest {
        files: Vec<FileEntry>,
        #[serde(default)]
        primary: bool,
    },
    File {
        file: FileEntry,
    },
    Context {
        records: Records,
    },
    ContextApplied {
        deferred: bool,
        message: Option<String>,
    },
}
