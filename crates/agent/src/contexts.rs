//! The computer's side of plugin context sync (docs/DESIGN.md section 8):
//! one adapter per kind of third-party data. The first is F95Checker's
//! `db.sqlite3`, for droidtop's F95 plugin.
//!
//! An adapter reads only the fields its context declares and writes only the
//! ones that may travel to the computer. It writes only while the app that
//! owns the data is closed: F95Checker keeps its database in memory and
//! writes it back, so a change made under it would be lost.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use droidtop_agent_core::context::{RecordChange, Records};
use rusqlite::types::Value as SqlValue;
use serde_json::Value;

use crate::scan::{columns, DbCopy};

pub trait Adapter: Send + Sync {
    fn pull(&self) -> Result<Records, String>;
    /// Ok(Some(reason)) when the changes must wait.
    fn push(&self, changes: Vec<RecordChange>) -> Result<Option<String>, String>;
}

pub fn adapter(id: &str) -> Option<Box<dyn Adapter>> {
    match id {
        "f95checker" => Some(Box::new(F95Checker { db: crate::scan::apps::f95checker_dir().join("db.sqlite3") })),
        _ => None,
    }
}

/// The contexts this agent can serve.
pub const KNOWN: &[(&str, &str)] = &[("f95checker", "F95Checker's watched threads, for the F95 plugin")];

pub struct F95Checker {
    pub db: PathBuf,
}

/// The `games` columns the context carries, and what kind each is. Never
/// the `cookies` table or the settings' passwords and tokens.
const FIELDS: &[(&str, Kind)] = &[
    ("name", Kind::Text),
    ("url", Kind::Text),
    ("version", Kind::Text),
    ("developer", Kind::Text),
    ("status", Kind::Int),
    ("type", Kind::Int),
    ("last_updated", Kind::Int),
    ("installed", Kind::Text),
    ("finished", Kind::Text),
    ("archived", Kind::Bool),
    ("rating", Kind::Int),
    ("notes", Kind::Text),
];

/// The fields the device may change on the computer.
const WRITABLE: &[&str] = &["installed", "finished", "archived", "rating", "notes"];

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Int,
    Bool,
}

fn to_json(kind: Kind, v: SqlValue) -> Value {
    match (kind, v) {
        (_, SqlValue::Null) => Value::Null,
        (Kind::Bool, SqlValue::Integer(i)) => Value::Bool(i != 0),
        (_, SqlValue::Integer(i)) => Value::from(i),
        (_, SqlValue::Real(f)) => Value::from(f),
        (_, SqlValue::Text(t)) => Value::String(t),
        (_, SqlValue::Blob(_)) => Value::Null,
    }
}

fn to_sql(kind: Kind, v: &Value) -> SqlValue {
    match (kind, v) {
        (_, Value::Null) => SqlValue::Null,
        (Kind::Bool, Value::Bool(b)) => SqlValue::Integer(i64::from(*b)),
        (Kind::Int, Value::Number(n)) => n.as_i64().map(SqlValue::Integer).unwrap_or(SqlValue::Null),
        (Kind::Text, Value::String(s)) => SqlValue::Text(s.clone()),
        (Kind::Text, other) => SqlValue::Text(other.to_string()),
        (_, Value::Bool(b)) => SqlValue::Integer(i64::from(*b)),
        (_, Value::Number(n)) => n.as_i64().map(SqlValue::Integer).unwrap_or(SqlValue::Null),
        _ => SqlValue::Null,
    }
}

/// Whether F95Checker is running on this computer.
pub fn f95checker_running() -> bool {
    let me = std::process::id().to_string();
    if cfg!(windows) {
        return Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("f95checker"))
            .unwrap_or(false);
    }
    if cfg!(target_os = "linux") {
        let Ok(read) = fs::read_dir("/proc") else { return false };
        return read.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.chars().all(|c| c.is_ascii_digit())
                && name != me
                && fs::read(e.path().join("cmdline"))
                    .map(|c| String::from_utf8_lossy(&c).to_lowercase())
                    .is_ok_and(|c| c.contains("f95checker") && !c.contains("droidtop-agent"))
        });
    }
    Command::new("ps")
        .args(["-axo", "command"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).to_lowercase().lines().any(|l| l.contains("f95checker") && !l.contains("droidtop-agent"))
        })
        .unwrap_or(false)
}

impl Adapter for F95Checker {
    fn pull(&self) -> Result<Records, String> {
        if !self.db.is_file() {
            return Err("F95Checker's database is not on this computer".into());
        }
        let copy = DbCopy::open(&self.db)?;
        let present = columns(&copy.conn, "games");
        let fields: Vec<(&str, Kind)> = FIELDS.iter().copied().filter(|(f, _)| present.iter().any(|p| p == f)).collect();
        let custom = if present.iter().any(|p| p == "custom") { "AND (custom IS NULL OR custom = 0)" } else { "" };
        let sql = format!(
            "SELECT id{}{} FROM games WHERE id > 0 {custom}",
            if fields.is_empty() { "" } else { ", " },
            fields.iter().map(|(f, _)| format!("\"{f}\"")).collect::<Vec<_>>().join(", ")
        );
        let mut stmt = copy.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let mut record = droidtop_agent_core::context::Record::new();
                for (i, (name, kind)) in fields.iter().enumerate() {
                    let v: SqlValue = row.get(i + 1)?;
                    record.insert(name.to_string(), to_json(*kind, v));
                }
                Ok((id.to_string(), record))
            })
            .map_err(|e| e.to_string())?;
        Ok(rows.flatten().collect())
    }

    fn push(&self, changes: Vec<RecordChange>) -> Result<Option<String>, String> {
        if changes.is_empty() {
            return Ok(None);
        }
        if f95checker_running() {
            return Ok(Some("F95Checker is open on the computer; the change is made there once it is closed".into()));
        }
        if !self.db.is_file() {
            return Err("F95Checker's database is not on this computer".into());
        }
        // One copy of the database as it was before this agent last wrote it.
        let backup = self.db.with_file_name("db.sqlite3.droidtop-agent-backup");
        fs::copy(&self.db, &backup).map_err(|e| format!("could not back up F95Checker's database: {e}"))?;
        let mut conn = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        let present = columns(&conn, "games");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for change in changes {
            match change {
                RecordChange::Upsert { key, fields } => {
                    let Ok(id) = key.parse::<i64>() else { continue };
                    if id <= 0 {
                        continue;
                    }
                    let exists: bool =
                        tx.query_row("SELECT EXISTS(SELECT 1 FROM games WHERE id = ?1)", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
                    if !exists {
                        let name =
                            fields.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("Unknown ({id})"));
                        let url = fields
                            .get("url")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("https://f95zone.to/threads/{id}/"));
                        let now = droidtop_agent_core::library::now_ms() / 1000;
                        // The columns F95Checker's own create_game sets for a thread.
                        tx.execute(
                            "INSERT INTO games (id, custom, name, url, added_on) VALUES (?1, 0, ?2, ?3, ?4)",
                            rusqlite::params![id, name, url, now],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    for (name, value) in &fields {
                        if !WRITABLE.contains(&name.as_str()) || !present.iter().any(|p| p == name) {
                            continue;
                        }
                        let Some((_, kind)) = FIELDS.iter().find(|(f, _)| f == name) else { continue };
                        tx.execute(&format!("UPDATE games SET \"{name}\" = ?1 WHERE id = ?2"), rusqlite::params![to_sql(*kind, value), id])
                            .map_err(|e| e.to_string())?;
                    }
                }
                RecordChange::Remove { key } => {
                    let Ok(id) = key.parse::<i64>() else { continue };
                    tx.execute("DELETE FROM games WHERE id = ?1 AND id > 0", [id]).map_err(|e| e.to_string())?;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dta-f95-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db.sqlite3");
        let conn = rusqlite::Connection::open(&db).unwrap();
        // The shape of F95Checker's games table (modules/db.py), trimmed to what is used here.
        conn.execute_batch(
            r#"CREATE TABLE games (id INTEGER PRIMARY KEY, custom INTEGER DEFAULT NULL, name TEXT DEFAULT "", version TEXT DEFAULT "Unchecked",
               url TEXT DEFAULT "", added_on INTEGER DEFAULT 0, installed TEXT DEFAULT "", finished TEXT DEFAULT "",
               archived INTEGER DEFAULT 0, rating INTEGER DEFAULT 0, notes TEXT DEFAULT "");
               CREATE TABLE cookies (key TEXT PRIMARY KEY, value TEXT DEFAULT "");
               INSERT INTO games (id, name, version, installed) VALUES (100, 'A Game', '0.2', '0.1');
               INSERT INTO games (id, custom, name) VALUES (-5, 1, 'Custom');
               INSERT INTO cookies VALUES ('xf_session', 'secret');"#,
        )
        .unwrap();
        db
    }

    #[test]
    fn reads_threads_and_writes_only_the_writable_fields() {
        let db = fixture();
        let a = F95Checker { db: db.clone() };
        let records = a.pull().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records["100"]["installed"], json!("0.1"));
        assert_eq!(records["100"]["archived"], json!(false));
        assert!(!serde_json::to_string(&records).unwrap().contains("secret"));
        let changes = vec![
            RecordChange::Upsert {
                key: "100".into(),
                fields: [("installed".to_string(), json!("0.2")), ("version".to_string(), json!("9.9"))].into_iter().collect(),
            },
            RecordChange::Upsert {
                key: "200".into(),
                fields: [("name".to_string(), json!("New")), ("archived".to_string(), json!(true))].into_iter().collect(),
            },
        ];
        if f95checker_running() {
            return;
        }
        assert_eq!(a.push(changes).unwrap(), None);
        let after = a.pull().unwrap();
        assert_eq!(after["100"]["installed"], json!("0.2"));
        assert_eq!(after["100"]["version"], json!("0.2"));
        assert_eq!(after["200"]["name"], json!("New"));
        assert_eq!(after["200"]["archived"], json!(true));
        assert!(db.with_file_name("db.sqlite3.droidtop-agent-backup").is_file());
        a.push(vec![RecordChange::Remove { key: "200".into() }]).unwrap();
        assert!(!a.pull().unwrap().contains_key("200"));
        let _ = fs::remove_dir_all(db.parent().unwrap());
    }
}
