//! The shared library (docs/DESIGN.md section 7).
//!
//! Two kinds of change, and neither can clobber the other side:
//! - **Install**: a device's facts about a game it has or had (installed,
//!   path, size, version, launcher, its own play time). Only that device
//!   writes them; a game it no longer has stays as `installed: false`.
//! - **Mark**: one of the person's own marks on a game (favourite, hidden,
//!   completion, rating, notes, tags, collections), last writer wins on a
//!   hybrid logical clock.
//!
//! Each device keeps the latest change per slot (a game and device, or a
//! game and field) under a sequence number; a peer pulls what is newer than
//! the cursor it holds, so a sync costs what changed, not the library's
//! size. A change that moves nothing is not logged again, so changes relayed
//! through a third device stop instead of bouncing.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keys::PeerId;

/// Wall time, a counter for changes in the same millisecond, and the device,
/// so two devices never make the same stamp.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Hlc {
    pub ms: i64,
    pub n: u32,
    pub dev: u64,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A device's number in clock stamps: the first 8 bytes of its PeerId.
pub fn device_tag(peer: &PeerId) -> u64 {
    u64::from_be_bytes(peer.0[..8].try_into().expect("8 bytes"))
}

/// A game as one device has it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Install {
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What owns or starts it there: `steam`, `gog`, `folder`, `retroarch`...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    #[serde(default)]
    pub play_seconds: u64,
    #[serde(default)]
    pub last_played_ms: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Install {
        game: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        platform: Option<String>,
        /// The PeerId of the device the facts are about, in hex.
        device: String,
        device_name: String,
        install: Install,
        at: Hlc,
    },
    Mark {
        game: String,
        field: String,
        value: Value,
        at: Hlc,
    },
}

impl Change {
    fn at(&self) -> Hlc {
        match self {
            Change::Install { at, .. } | Change::Mark { at, .. } => *at,
        }
    }

    fn slot(&self) -> String {
        match self {
            Change::Install { game, device, .. } => format!("i\u{0}{game}\u{0}{device}"),
            Change::Mark { game, field, .. } => format!("m\u{0}{game}\u{0}{field}"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DeviceInstall {
    pub device_name: String,
    pub install: Install,
    pub at: Hlc,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MarkValue {
    pub value: Value,
    pub at: Hlc,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct GameRecord {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// By device PeerId (hex).
    #[serde(default)]
    pub installs: BTreeMap<String, DeviceInstall>,
    #[serde(default)]
    pub marks: BTreeMap<String, MarkValue>,
    #[serde(skip)]
    title_at: Hlc,
}

impl GameRecord {
    /// Play time on every device together.
    pub fn play_seconds(&self) -> u64 {
        self.installs.values().map(|d| d.install.play_seconds).sum()
    }

    pub fn last_played_ms(&self) -> i64 {
        self.installs.values().map(|d| d.install.last_played_ms).max().unwrap_or(0)
    }
}

/// How far this device has exchanged changes with one peer.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    /// The peer's sequence number this device has pulled up to.
    pub pulled: u64,
    /// This device's sequence number the peer has been sent up to.
    pub pushed: u64,
}

/// A game a scan found on this device.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ScannedGame {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub platform: Option<String>,
    pub install: Install,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Library {
    pub games: BTreeMap<String, GameRecord>,
    #[serde(default)]
    log: BTreeMap<String, (u64, Change)>,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    clock: Hlc,
    /// By peer PeerId (hex).
    #[serde(default)]
    pub cursors: BTreeMap<String, Cursor>,
}

impl Library {
    pub fn load(path: &Path) -> io::Result<Library> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Library::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?)?;
        fs::rename(&tmp, path)
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// A new stamp for a change this device makes.
    pub fn tick(&mut self, dev: u64) -> Hlc {
        let now = now_ms();
        self.clock = if now > self.clock.ms { Hlc { ms: now, n: 0, dev } } else { Hlc { ms: self.clock.ms, n: self.clock.n + 1, dev } };
        self.clock
    }

    /// Applies a change from anywhere; true when it moved something.
    pub fn apply(&mut self, change: Change) -> bool {
        let at = change.at();
        if at > self.clock {
            self.clock = Hlc { ms: at.ms, n: at.n, dev: self.clock.dev };
        }
        let changed = match &change {
            Change::Install { game, title, platform, device, device_name, install, at } => {
                let record = self.games.entry(game.clone()).or_default();
                let newer = record.installs.get(device).is_none_or(|d| *at > d.at);
                if newer {
                    record
                        .installs
                        .insert(device.clone(), DeviceInstall { device_name: device_name.clone(), install: install.clone(), at: *at });
                    if !title.is_empty() && *at >= record.title_at {
                        record.title = title.clone();
                        record.title_at = *at;
                        if platform.is_some() {
                            record.platform = platform.clone();
                        }
                    }
                }
                newer
            }
            Change::Mark { game, field, value, at } => {
                let record = self.games.entry(game.clone()).or_default();
                let newer = record.marks.get(field).is_none_or(|m| *at > m.at);
                if newer {
                    record.marks.insert(field.clone(), MarkValue { value: value.clone(), at: *at });
                }
                newer
            }
        };
        if changed {
            self.seq += 1;
            self.log.insert(change.slot(), (self.seq, change));
        }
        changed
    }

    /// What changed after [`since`], oldest first, and the cursor to ask from next.
    pub fn changes_since(&self, since: u64) -> (Vec<Change>, u64) {
        let mut out: Vec<&(u64, Change)> = self.log.values().filter(|(s, _)| *s > since).collect();
        out.sort_by_key(|(s, _)| *s);
        (out.into_iter().map(|(_, c)| c.clone()).collect(), self.seq)
    }

    /// Records what a scan of [`device`] found: new and changed games as
    /// install changes, and games it had that are gone as `installed: false`
    /// (their play time kept). Returns how many changes it made.
    pub fn update_device(&mut self, device: &PeerId, device_name: &str, games: Vec<ScannedGame>) -> usize {
        let dev = device_tag(device);
        let id = device.to_hex();
        let mut seen = std::collections::BTreeSet::new();
        let mut count = 0;
        for g in games {
            seen.insert(g.key.clone());
            let current = self.games.get(&g.key);
            // The title is left out: two devices may word it differently,
            // and comparing it would have them overwrite each other on
            // every scan.
            let same = current.is_some_and(|r| r.installs.get(&id).is_some_and(|d| d.install == g.install && d.device_name == device_name));
            if same {
                continue;
            }
            let at = self.tick(dev);
            let change = Change::Install {
                game: g.key,
                title: g.title,
                platform: g.platform,
                device: id.clone(),
                device_name: device_name.to_string(),
                install: g.install,
                at,
            };
            if self.apply(change) {
                count += 1;
            }
        }
        let gone: Vec<(String, String, Install)> = self
            .games
            .iter()
            .filter(|(key, _)| !seen.contains(*key))
            .filter_map(|(key, r)| {
                r.installs.get(&id).filter(|d| d.install.installed).map(|d| (key.clone(), r.title.clone(), d.install.clone()))
            })
            .collect();
        for (key, title, install) in gone {
            let at = self.tick(dev);
            let install = Install { installed: false, path: None, size: 0, ..install };
            if self.apply(Change::Install {
                game: key,
                title,
                platform: None,
                device: id.clone(),
                device_name: device_name.to_string(),
                install,
                at,
            }) {
                count += 1;
            }
        }
        count
    }

    /// Sets one of the person's marks on a game, made on [`device`].
    pub fn mark(&mut self, device: &PeerId, game: &str, field: &str, value: Value) -> bool {
        let at = self.tick(device_tag(device));
        self.apply(Change::Mark { game: game.to_string(), field: field.to_string(), value, at })
    }
}

/// The key a game without a store id goes by: its title in lower case with
/// only letters and digits, so "The Game: Deluxe!" and "the game deluxe"
/// meet.
pub fn title_key(title: &str) -> String {
    let words: Vec<String> =
        title.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    format!("title:{}", words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::DeviceKey;

    fn game(key: &str, installed: bool) -> ScannedGame {
        ScannedGame { key: key.into(), title: key.into(), platform: None, install: Install { installed, size: 10, ..Default::default() } }
    }

    #[test]
    fn scans_become_changes_and_removals_become_tombstones() {
        let pc = DeviceKey::generate().peer_id();
        let mut lib = Library::default();
        assert_eq!(lib.update_device(&pc, "PC", vec![game("steam:1", true), game("steam:2", true)]), 2);
        assert_eq!(lib.update_device(&pc, "PC", vec![game("steam:1", true), game("steam:2", true)]), 0);
        assert_eq!(lib.update_device(&pc, "PC", vec![game("steam:1", true)]), 1);
        let gone = &lib.games["steam:2"].installs[&pc.to_hex()];
        assert!(!gone.install.installed);
    }

    #[test]
    fn two_devices_converge_and_relays_stop() {
        let pc = DeviceKey::generate().peer_id();
        let hh = DeviceKey::generate().peer_id();
        let mut a = Library::default();
        let mut b = Library::default();
        a.update_device(&pc, "PC", vec![game("steam:1", true)]);
        b.update_device(&hh, "Handheld", vec![game("steam:1", true), game("gog:9", true)]);
        b.mark(&hh, "steam:1", "favourite", Value::Bool(true));
        a.mark(&pc, "steam:1", "favourite", Value::Bool(false));
        let (from_a, _) = a.changes_since(0);
        let (from_b, _) = b.changes_since(0);
        for c in from_b.clone() {
            a.apply(c);
        }
        for c in from_a {
            b.apply(c);
        }
        assert_eq!(a.games["steam:1"].installs.len(), 2);
        assert_eq!(a.games["steam:1"].marks["favourite"], b.games["steam:1"].marks["favourite"]);
        // Sending b's changes again moves nothing and logs nothing.
        let seq = a.seq();
        for c in from_b {
            assert!(!a.apply(c));
        }
        assert_eq!(a.seq(), seq);
    }

    #[test]
    fn title_keys_ignore_punctuation_and_case() {
        assert_eq!(title_key("The Game: Deluxe!"), title_key("the  game deluxe"));
    }
}
