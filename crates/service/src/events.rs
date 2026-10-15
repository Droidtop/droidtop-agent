//! What the running agent does, for the program that hosts it: a handheld
//! asking to start a game, a session with a handheld, saves and library
//! changes synced, a game folder arriving, and what went wrong. Each event
//! is printed as the command line always printed it, and kept in a bounded
//! list, numbered, that a host reads from where it left off
//! (`droidtop_agent.service.events` for Pythia), so a host can log and show
//! it (docs/DESIGN.md section 16).

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

/// How many events are kept: the oldest go first.
pub const KEPT: usize = 500;

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warn,
}

/// One thing the agent did.
#[derive(Serialize, Debug, Clone)]
pub struct Event {
    /// Numbered from 1, in order, for as long as the process runs.
    pub seq: u64,
    /// When, in milliseconds since 1970.
    pub ms: u64,
    pub level: Level,
    /// `listening`, `session`, `launch`, `saves`, `library`, `arrived`,
    /// `announce`, `discovery`, `wireguard`, `rendezvous`, `share` or `manager`.
    pub kind: &'static str,
    /// The line the command line prints, in words a screen can show.
    pub message: String,
    /// What the event is about, by name: `peer` and `device` (the handheld's
    /// id and name), `title` and `key` (the game), `changes`, `outcome`...
    #[serde(flatten)]
    pub detail: Map<String, Value>,
}

#[derive(Default)]
pub struct Events {
    kept: Mutex<(u64, VecDeque<Event>)>,
    arrived: Condvar,
}

impl Events {
    pub fn info(&self, kind: &'static str, message: String, detail: Value) {
        self.emit(Level::Info, kind, message, detail);
    }

    pub fn warn(&self, kind: &'static str, message: String, detail: Value) {
        self.emit(Level::Warn, kind, message, detail);
    }

    fn emit(&self, level: Level, kind: &'static str, message: String, detail: Value) {
        match level {
            Level::Info => println!("{message}"),
            Level::Warn => eprintln!("{message}"),
        }
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let detail = match detail {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        let mut kept = self.kept.lock().unwrap();
        kept.0 += 1;
        let seq = kept.0;
        kept.1.push_back(Event { seq, ms, level, kind, message, detail });
        while kept.1.len() > KEPT {
            kept.1.pop_front();
        }
        self.arrived.notify_all();
    }

    /// The events after number [since] that are still kept, oldest first
    /// (0: all of them).
    pub fn since(&self, since: u64) -> Vec<Event> {
        self.kept.lock().unwrap().1.iter().filter(|e| e.seq > since).cloned().collect()
    }

    /// [`since`](Events::since), waiting up to [wait] for one when there is none yet.
    pub fn wait_since(&self, since: u64, wait: Duration) -> Vec<Event> {
        let until = Instant::now() + wait;
        let mut kept = self.kept.lock().unwrap();
        while kept.0 <= since {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            kept = self.arrived.wait_timeout(kept, left).unwrap().0;
        }
        kept.1.iter().filter(|e| e.seq > since).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn kept_numbered_and_bounded() {
        let events = Events::default();
        assert!(events.since(0).is_empty());
        events.info("launch", "Deck asked to start Hades: Started".into(), json!({ "device": "Deck", "key": "steam:1145360" }));
        events.warn("session", "Session with ab12 ended: closed".into(), json!({}));
        let all = events.since(0);
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(all[1].level, Level::Warn);
        let first = serde_json::to_value(&all[0]).unwrap();
        assert_eq!(first["kind"], "launch");
        assert_eq!(first["level"], "info");
        assert_eq!(first["key"], "steam:1145360");
        assert_eq!(events.since(1).len(), 1);
        assert!(events.since(2).is_empty());

        for i in 0..KEPT + 10 {
            events.info("library", format!("Library: {i} changes from the scan."), json!({}));
        }
        let all = events.since(0);
        assert_eq!(all.len(), KEPT);
        assert_eq!(all.last().unwrap().seq, (KEPT + 12) as u64);
        assert_eq!(all[0].seq, 13);
    }

    #[test]
    fn a_waiting_reader_wakes_on_the_next_event() {
        let events = Arc::new(Events::default());
        assert!(events.wait_since(0, Duration::from_millis(20)).is_empty());
        let e = events.clone();
        let reader = std::thread::spawn(move || e.wait_since(0, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(50));
        events.info("arrived", "Hades arrived in /games/Hades.".into(), json!({}));
        let got = reader.join().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, "arrived");
    }
}
