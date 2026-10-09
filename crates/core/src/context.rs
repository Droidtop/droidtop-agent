//! Plugin context sync (docs/DESIGN.md section 8; droidtop's
//! docs/plugin-api.md, "Context sync"). A context is a set of records, each
//! a map of fields; the plugin declares which fields sync in which direction
//! and how a field both sides changed is settled. The merge is three-way per
//! field against the baseline the handheld keeps per computer and context.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Record = BTreeMap<String, Value>;
pub type Records = BTreeMap<String, Record>;

/// Which way a field (or a record's presence) travels.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Both,
    /// From the computer to the device only.
    ToDevice,
    /// From the device to the computer only.
    ToComputer,
}

/// Who wins a field both sides changed differently.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    Device,
    Computer,
    Ask,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub direction: Direction,
    #[serde(default = "ask")]
    pub rule: Rule,
}

fn ask() -> Rule {
    Rule::Ask
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ContextDecl {
    pub id: String,
    pub fields: Vec<FieldDecl>,
    /// Which way adding and removing whole records travels.
    pub presence: Direction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RecordChange {
    /// Sets these fields (adding the record when it is not there).
    Upsert {
        key: String,
        fields: Record,
    },
    Remove {
        key: String,
    },
}

/// A field both sides changed, left as it was for the person to settle.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FieldConflict {
    pub key: String,
    pub field: String,
    pub device: Option<Value>,
    pub computer: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Merge {
    /// The device's records after the merge.
    pub device: Records,
    /// What the computer must apply.
    pub to_computer: Vec<RecordChange>,
    pub conflicts: Vec<FieldConflict>,
    /// The baseline to keep once the computer applied [`to_computer`].
    pub baseline: Records,
}

fn sends_to_computer(d: Direction) -> bool {
    matches!(d, Direction::Both | Direction::ToComputer)
}

fn sends_to_device(d: Direction) -> bool {
    matches!(d, Direction::Both | Direction::ToDevice)
}

/// Only the declared fields of a record.
fn declared(decl: &ContextDecl, record: &Record) -> Record {
    decl.fields.iter().filter_map(|f| record.get(&f.name).map(|v| (f.name.clone(), v.clone()))).collect()
}

pub fn merge(decl: &ContextDecl, device: &Records, computer: &Records, baseline: &Records) -> Merge {
    let mut out = Merge { device: device.clone(), ..Default::default() };
    let keys: BTreeSet<&String> = device.keys().chain(computer.keys()).chain(baseline.keys()).collect();
    for key in keys {
        let (d, c, b) = (device.get(key), computer.get(key), baseline.get(key));
        match (d, c) {
            (Some(d), Some(c)) => merge_record(decl, key, d, c, b, &mut out),
            (Some(d), None) => {
                if b.is_some() {
                    // The computer removed it.
                    if sends_to_device(decl.presence) {
                        out.device.remove(key);
                    } else {
                        out.to_computer.push(RecordChange::Upsert { key: key.clone(), fields: to_computer_fields(decl, d) });
                        out.baseline.insert(key.clone(), declared(decl, d));
                    }
                } else if sends_to_computer(decl.presence) {
                    // The device added it.
                    out.to_computer.push(RecordChange::Upsert { key: key.clone(), fields: to_computer_fields(decl, d) });
                    out.baseline.insert(key.clone(), declared(decl, d));
                }
            }
            (None, Some(c)) => {
                if b.is_some() {
                    // The device removed it.
                    if sends_to_computer(decl.presence) {
                        out.to_computer.push(RecordChange::Remove { key: key.clone() });
                    } else {
                        out.device.insert(key.clone(), declared(decl, c));
                        out.baseline.insert(key.clone(), declared(decl, c));
                    }
                } else if sends_to_device(decl.presence) {
                    // The computer added it.
                    out.device.insert(key.clone(), declared(decl, c));
                    out.baseline.insert(key.clone(), declared(decl, c));
                }
            }
            (None, None) => {}
        }
    }
    out
}

fn to_computer_fields(decl: &ContextDecl, record: &Record) -> Record {
    decl.fields
        .iter()
        .filter(|f| sends_to_computer(f.direction))
        .filter_map(|f| record.get(&f.name).map(|v| (f.name.clone(), v.clone())))
        .collect()
}

fn merge_record(decl: &ContextDecl, key: &str, d: &Record, c: &Record, b: Option<&Record>, out: &mut Merge) {
    let mut device = d.clone();
    let mut push = Record::new();
    let mut base = Record::new();
    for f in &decl.fields {
        let name = &f.name;
        let (dv, cv) = (d.get(name), c.get(name));
        let bv = b.and_then(|r| r.get(name));
        let take_computer = |device: &mut Record, base: &mut Record| {
            match cv {
                Some(v) => device.insert(name.clone(), v.clone()),
                None => device.remove(name),
            };
            if let Some(v) = cv {
                base.insert(name.clone(), v.clone());
            }
        };
        match f.direction {
            Direction::ToDevice => take_computer(&mut device, &mut base),
            Direction::ToComputer => {
                if dv != cv {
                    if let Some(v) = dv {
                        push.insert(name.clone(), v.clone());
                    }
                }
                if let Some(v) = dv {
                    base.insert(name.clone(), v.clone());
                }
            }
            Direction::Both => {
                if dv == cv {
                    if let Some(v) = dv {
                        base.insert(name.clone(), v.clone());
                    }
                } else if dv == bv {
                    take_computer(&mut device, &mut base);
                } else if cv == bv {
                    if let Some(v) = dv {
                        push.insert(name.clone(), v.clone());
                        base.insert(name.clone(), v.clone());
                    }
                } else {
                    match f.rule {
                        Rule::Device => {
                            if let Some(v) = dv {
                                push.insert(name.clone(), v.clone());
                                base.insert(name.clone(), v.clone());
                            }
                        }
                        Rule::Computer => take_computer(&mut device, &mut base),
                        Rule::Ask => {
                            out.conflicts.push(FieldConflict {
                                key: key.to_string(),
                                field: name.clone(),
                                device: dv.cloned(),
                                computer: cv.cloned(),
                            });
                            if let Some(v) = bv {
                                base.insert(name.clone(), v.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    out.device.insert(key.to_string(), device);
    if !push.is_empty() {
        out.to_computer.push(RecordChange::Upsert { key: key.to_string(), fields: push });
    }
    out.baseline.insert(key.to_string(), base);
}

/// The baseline to keep when the computer could not apply [`to_computer`]
/// yet (the app that owns the data is running): those fields keep the
/// computer's values, so the next sync sees them as the device's changes
/// again.
pub fn baseline_when_deferred(merge: &Merge, computer: &Records) -> Records {
    let mut base = merge.baseline.clone();
    for change in &merge.to_computer {
        match change {
            RecordChange::Upsert { key, fields } => match computer.get(key) {
                Some(c) => {
                    let entry = base.entry(key.clone()).or_default();
                    for name in fields.keys() {
                        match c.get(name) {
                            Some(v) => entry.insert(name.clone(), v.clone()),
                            None => entry.remove(name),
                        };
                    }
                }
                None => {
                    base.remove(key);
                }
            },
            RecordChange::Remove { key } => {
                if let Some(c) = computer.get(key) {
                    base.insert(key.clone(), c.clone());
                }
            }
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl() -> ContextDecl {
        ContextDecl {
            id: "f95checker".into(),
            fields: vec![
                FieldDecl { name: "name".into(), direction: Direction::ToDevice, rule: Rule::Computer },
                FieldDecl { name: "installed".into(), direction: Direction::Both, rule: Rule::Ask },
                FieldDecl { name: "rating".into(), direction: Direction::Both, rule: Rule::Device },
            ],
            presence: Direction::Both,
        }
    }

    fn rec(pairs: &[(&str, Value)]) -> Record {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn set(entries: &[(&str, Record)]) -> Records {
        entries.iter().map(|(k, r)| (k.to_string(), r.clone())).collect()
    }

    #[test]
    fn one_sided_changes_travel_both_ways() {
        let base = set(&[("1", rec(&[("name", json!("A")), ("installed", json!("0.1")), ("rating", json!(3))]))]);
        let device = set(&[("1", rec(&[("name", json!("A")), ("installed", json!("0.2")), ("rating", json!(3))]))]);
        let computer = set(&[("1", rec(&[("name", json!("A v2")), ("installed", json!("0.1")), ("rating", json!(5))]))]);
        let m = merge(&decl(), &device, &computer, &base);
        assert_eq!(m.device["1"]["name"], json!("A v2"));
        assert_eq!(m.device["1"]["rating"], json!(5));
        assert_eq!(m.to_computer, vec![RecordChange::Upsert { key: "1".into(), fields: rec(&[("installed", json!("0.2"))]) }]);
        assert!(m.conflicts.is_empty());
    }

    #[test]
    fn both_changed_follows_the_rule() {
        let base = set(&[("1", rec(&[("installed", json!("0.1")), ("rating", json!(3))]))]);
        let device = set(&[("1", rec(&[("installed", json!("0.2")), ("rating", json!(4))]))]);
        let computer = set(&[("1", rec(&[("installed", json!("0.3")), ("rating", json!(5))]))]);
        let m = merge(&decl(), &device, &computer, &base);
        assert_eq!(m.conflicts.len(), 1);
        assert_eq!(m.conflicts[0].field, "installed");
        assert_eq!(m.to_computer, vec![RecordChange::Upsert { key: "1".into(), fields: rec(&[("rating", json!(4))]) }]);
        assert_eq!(m.baseline["1"]["installed"], json!("0.1"));
    }

    #[test]
    fn records_come_and_go_on_either_side() {
        let base = set(&[("gone-on-device", rec(&[])), ("gone-on-computer", rec(&[]))]);
        let device = set(&[("new-on-device", rec(&[("installed", json!("1"))])), ("gone-on-computer", rec(&[]))]);
        let computer = set(&[("new-on-computer", rec(&[("name", json!("N"))])), ("gone-on-device", rec(&[]))]);
        let m = merge(&decl(), &device, &computer, &base);
        assert!(m.device.contains_key("new-on-computer"));
        assert!(!m.device.contains_key("gone-on-computer"));
        assert!(m.to_computer.contains(&RecordChange::Remove { key: "gone-on-device".into() }));
        assert!(m.to_computer.iter().any(|c| matches!(c, RecordChange::Upsert { key, .. } if key == "new-on-device")));
    }

    #[test]
    fn a_deferred_push_is_pushed_again_next_time() {
        let base = set(&[("1", rec(&[("installed", json!("0.1"))]))]);
        let device = set(&[("1", rec(&[("installed", json!("0.2"))]))]);
        let computer = base.clone();
        let m = merge(&decl(), &device, &computer, &base);
        let kept = baseline_when_deferred(&m, &computer);
        let again = merge(&decl(), &m.device, &computer, &kept);
        assert_eq!(again.to_computer, m.to_computer);
    }
}
