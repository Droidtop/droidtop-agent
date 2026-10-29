//! The messages on the channel (docs/DESIGN.md section 5). The handheld
//! asks, the computer answers, one reply per request. File contents follow
//! the JSON header that announces them as raw messages of at most
//! [`CHUNK`] bytes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::{AdapterOffer, RecordChange, Records};
use crate::library::Change;
use crate::manifest::FileEntry;
use crate::saves::SaveSpec;

/// The largest piece of a file's contents in one message.
pub const CHUNK: usize = 1024 * 1024;

/// What a device publishes to the devices paired with it, per plugin: each
/// plugin's id and its small value (the Syncthing plugin's
/// `{"syncthing": "<device ID>"}`), hello feature `announce`
/// (Droidtop/tracker#556, #553). droidtop shows each plugin only its own.
pub type Announcements = BTreeMap<String, Value>;

/// The most a plugin's value may take, as JSON.
pub const MAX_ANNOUNCED: usize = 4096;

/// The most plugins one device announces for.
pub const MAX_PLUGINS: usize = 64;

/// Why [`a`] cannot be announced, if it cannot: too many plugins, a plugin
/// id that is not 1 to 128 of `A-Z a-z 0-9 . _ -`, or a value over
/// [`MAX_ANNOUNCED`] bytes.
pub fn check_announcements(a: &Announcements) -> Result<(), String> {
    if a.len() > MAX_PLUGINS {
        return Err(format!("at most {MAX_PLUGINS} plugins announce"));
    }
    for (plugin, value) in a {
        let named = (1..=128).contains(&plugin.len()) && plugin.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        if !named {
            return Err(format!("{plugin:?} is not a plugin id"));
        }
        if value.to_string().len() > MAX_ANNOUNCED {
            return Err(format!("{plugin}'s value is over {MAX_ANNOUNCED} bytes"));
        }
    }
    Ok(())
}

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
    /// A game's folder on the computer: its files, to copy it here
    /// (crate::gamecopy).
    GameFiles {
        game: GameRef,
    },
    /// One file of that folder from [`offset`]; the reply is a
    /// `game_file` header, the rest of the file, an empty message and a
    /// `digest` of the whole file.
    GameFileGet {
        game: GameRef,
        path: String,
        offset: u64,
    },
    /// This device's folder of a game is coming, to be made among the
    /// computer's game folders as [`folder`]'s name.
    GamePutStart {
        game: GameRef,
        folder: crate::gamecopy::GameFolder,
    },
    /// One file of it follows, as a `game_file` header, its pieces, an
    /// empty message and a `digest`.
    GameFilePut {
        game: GameRef,
        path: String,
    },
    /// Every file has been sent: put the folder in place.
    GamePutFinish {
        game: GameRef,
    },
    /// Start [`game`] on the computer (hello feature `launch`). The computer
    /// builds what it runs from the key itself and never executes text from
    /// the handheld; it answers [`Response::Launch`].
    LaunchGame {
        game: GameRef,
    },
    /// This device's announcements, all of them, in place of those it sent
    /// before (hello feature `announce`); the other answers
    /// [`Response::Announced`] with its own.
    Announce {
        values: Announcements,
    },
    Bye,
}

/// What the computer did with a [`Request::LaunchGame`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Launch {
    /// The game was asked to start: by Steam, which may still be waiting for
    /// a sign-in or an update before it runs, or by the program hosting the
    /// agent (a desktop manager such as Pythia) for a game of its own.
    /// `stream` says how the handheld shows it: `"windowcast"` for a window
    /// windowcast can stream; absent, Steam Link as before. `address` is
    /// where that host's windowcast listens (`HOST:PORT`), for a host that
    /// knows; absent, the handheld assumes windowcast's default port on the
    /// address it reached the computer at.
    Started {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
    },
    /// The person has not let this handheld start games on this computer.
    NotAllowed,
    /// The key is not one the computer can start, or the computer has not
    /// got the game installed.
    UnknownGame,
    /// The Steam client is not running, so nothing was started.
    SteamNotRunning,
    /// The computer could not hand the game to Steam.
    Failed { message: String },
}

/// What a device offers one other device (docs/DESIGN.md section 20):
/// nothing unless the person, or a handheld's pairing, set it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Offer {
    /// Its library: the other device may read it and copy any game in it.
    #[serde(default)]
    pub library: bool,
    /// The saves of those games: the other device may read and write them.
    #[serde(default)]
    pub saves: bool,
}

impl Offer {
    pub const NONE: Offer = Offer { library: false, saves: false };
    /// What a computer offers a handheld unless the person changed it.
    pub const LIBRARY_AND_SAVES: Offer = Offer { library: true, saves: true };
}

/// The `code` of an [`Response::Error`] for a request the device does not
/// offer to the one that asked.
pub const NOT_OFFERED: &str = "not_offered";

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Response {
    /// The answer to [`Request::LaunchGame`].
    Launch {
        outcome: Launch,
    },
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
        /// A reason a program can act on ([`NOT_OFFERED`]); absent for others.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
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
    GameFiles {
        folder: crate::gamecopy::GameFolder,
    },
    /// A file's header: its whole size and the offset its pieces start at.
    GameFile {
        path: String,
        size: u64,
        offset: u64,
    },
    /// The SHA-256 of a whole file that was just sent.
    Digest {
        sha256: String,
    },
    /// Per announced file, what the computer already has: its size when it
    /// is whole, `u64::MAX` when it starts over, else the bytes it holds.
    GamePutHave {
        have: Vec<u64>,
    },
    /// Where the computer put the folder.
    GamePlaced {
        folder: String,
    },
    /// The answering device's own announcements.
    Announced {
        values: Announcements,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes on the wire: a handheld and an agent built apart agree on them.
    #[test]
    fn a_launch_request_and_its_answers_have_a_fixed_shape() {
        let ask = Request::LaunchGame { game: GameRef { key: "steam:440".into(), title: "TF2".into() } };
        assert_eq!(serde_json::to_string(&ask).unwrap(), r#"{"t":"launch_game","game":{"key":"steam:440","title":"TF2"}}"#);
        for (launch, json) in [
            (Launch::Started { stream: None, address: None }, r#"{"t":"launch","outcome":{"result":"started"}}"#),
            (
                Launch::Started { stream: Some("windowcast".into()), address: None },
                r#"{"t":"launch","outcome":{"result":"started","stream":"windowcast"}}"#,
            ),
            (
                Launch::Started { stream: Some("windowcast".into()), address: Some("192.168.1.20:47100".into()) },
                r#"{"t":"launch","outcome":{"result":"started","stream":"windowcast","address":"192.168.1.20:47100"}}"#,
            ),
            (Launch::NotAllowed, r#"{"t":"launch","outcome":{"result":"not_allowed"}}"#),
            (Launch::UnknownGame, r#"{"t":"launch","outcome":{"result":"unknown_game"}}"#),
            (Launch::SteamNotRunning, r#"{"t":"launch","outcome":{"result":"steam_not_running"}}"#),
            (Launch::Failed { message: "no".into() }, r#"{"t":"launch","outcome":{"result":"failed","message":"no"}}"#),
        ] {
            let reply = Response::Launch { outcome: launch.clone() };
            assert_eq!(serde_json::to_string(&reply).unwrap(), json);
            match serde_json::from_str::<Response>(json).unwrap() {
                Response::Launch { outcome } => assert_eq!(outcome, launch),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn announcements_have_a_fixed_shape_and_limits() {
        let values: Announcements = [("syncthing".to_string(), serde_json::json!({ "syncthing": "MFZWI3D-BONSGYC" }))].into();
        let ask = Request::Announce { values: values.clone() };
        assert_eq!(serde_json::to_string(&ask).unwrap(), r#"{"t":"announce","values":{"syncthing":{"syncthing":"MFZWI3D-BONSGYC"}}}"#);
        let reply = Response::Announced { values: values.clone() };
        assert_eq!(serde_json::to_string(&reply).unwrap(), r#"{"t":"announced","values":{"syncthing":{"syncthing":"MFZWI3D-BONSGYC"}}}"#);
        assert!(check_announcements(&values).is_ok());
        assert!(check_announcements(&[("a b".to_string(), Value::Null)].into()).is_err());
        assert!(check_announcements(&[("big".to_string(), Value::String("x".repeat(MAX_ANNOUNCED)))].into()).is_err());
        let many: Announcements = (0..=MAX_PLUGINS).map(|i| (format!("p{i}"), Value::Null)).collect();
        assert!(check_announcements(&many).is_err());
    }
}
