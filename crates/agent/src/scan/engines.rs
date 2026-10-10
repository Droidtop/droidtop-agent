//! Which engine a game folder was made with, decided by droidtop's engines
//! database (`data/engines-database.json`, a copy of droidtop-platforms'
//! file): the same rules, in the same row order, that droidtop and
//! Enginehost classify with (droidtop SPEC 7e2b), so the computer and the
//! handheld agree on what a folder is. Only the byte probes the database
//! names as `builtin` (Godot's embedded pack, a Twine page, Unity's player)
//! are code, as they are in droidtop's `GameEngineDetector`.
//!
//! A rule that reads below its folder (`anyFileExtensionDeep` deeper than
//! the folder itself, Unity's player search) proves a game is somewhere
//! below, not that the folder is its root; the walk in `folders` uses that
//! difference the way droidtop's does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};
use serde_json::Value;

const DATABASE: &str = include_str!("../../data/engines-database.json");

/// One row of the database: an engine and the rules that recognise it.
pub struct Engine {
    pub id: String,
    rules: Vec<Rule>,
}

struct Rule {
    all: Vec<Condition>,
    /// Whether the rule reads an unnamed subtree below the folder.
    subtree: bool,
}

enum Condition {
    DirExists(String),
    FileExists(String),
    NameContains(String),
    Extension(String),
    ExtensionDeep(String, usize),
    NameIn(Vec<String>),
    DirPrefixCount(String, usize),
    FileHead(String, Regex),
    Builtin(String),
}

impl Condition {
    fn reads_subtree(&self) -> bool {
        match self {
            Condition::ExtensionDeep(_, depth) => *depth > 0,
            Condition::Builtin(name) => name == "unity",
            _ => false,
        }
    }
}

/// The database this build carries.
pub fn database() -> &'static [Engine] {
    static DB: OnceLock<Vec<Engine>> = OnceLock::new();
    DB.get_or_init(|| parse(DATABASE))
}

/// The rows of an engines database. A condition type this build does not
/// know drops its rule rather than matching (a newer database must not
/// misdetect), as in droidtop's parser.
pub fn parse(text: &str) -> Vec<Engine> {
    let Ok(value) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    for row in value["engines"].as_array().into_iter().flatten() {
        let Some(id) = row["id"].as_str() else { continue };
        let mut rules = Vec::new();
        for rule in row["detect"].as_array().into_iter().flatten() {
            let conditions: Option<Vec<Condition>> = rule["all"].as_array().into_iter().flatten().map(condition).collect();
            if let Some(all) = conditions.filter(|all| !all.is_empty()) {
                let subtree = all.iter().any(Condition::reads_subtree);
                rules.push(Rule { all, subtree });
            }
        }
        out.push(Engine { id: id.to_string(), rules });
    }
    out
}

fn condition(c: &Value) -> Option<Condition> {
    let text = |key: &str| c[key].as_str().map(str::to_string);
    let lower = |key: &str| c[key].as_str().map(str::to_lowercase);
    Some(match c["type"].as_str()? {
        "dirExists" => Condition::DirExists(text("path")?),
        "fileExists" => Condition::FileExists(text("path")?),
        "anyFileNameContains" => Condition::NameContains(lower("value")?),
        "anyFileExtension" => Condition::Extension(lower("value")?),
        "anyFileExtensionDeep" => Condition::ExtensionDeep(lower("value")?, c["maxDepth"].as_u64()? as usize),
        "anyFileNameIn" => Condition::NameIn(c["values"].as_array()?.iter().filter_map(|v| v.as_str().map(str::to_lowercase)).collect()),
        "dirNamePrefixCount" => Condition::DirPrefixCount(lower("prefix")?, c["min"].as_u64()? as usize),
        "fileHeadRegex" => Condition::FileHead(text("path")?, RegexBuilder::new(c["regex"].as_str()?).case_insensitive(true).build().ok()?),
        "builtin" => Condition::Builtin(text("name")?),
        _ => return None,
    })
}

/// A folder's entries, read once: names of its files and its folders.
pub struct Listing {
    pub dir: PathBuf,
    pub files: Vec<String>,
    pub dirs: Vec<String>,
}

impl Listing {
    fn read(dir: &Path) -> Listing {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        if let Ok(read) = fs::read_dir(dir) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let kind = match entry.file_type() {
                    Ok(t) if t.is_symlink() => fs::metadata(entry.path()).map(|m| m.file_type()).ok(),
                    Ok(t) => Some(t),
                    Err(_) => None,
                };
                match kind {
                    Some(t) if t.is_dir() => dirs.push(name),
                    Some(t) if t.is_file() => files.push(name),
                    _ => {}
                }
            }
        }
        // Name order, so a walk reads the same way on every scan.
        files.sort_by_key(|n| n.to_lowercase());
        dirs.sort_by_key(|n| n.to_lowercase());
        Listing { dir: dir.to_path_buf(), files, dirs }
    }

    /// The real name of an entry, compared without case.
    fn entry<'a>(names: &'a [String], wanted: &str) -> Option<&'a String> {
        names.iter().find(|n| n.eq_ignore_ascii_case(wanted))
    }
}

/// The extension of a file name, lowercased; empty when there is none.
pub fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

/// Which rules a question asks.
#[derive(Clone, Copy, PartialEq)]
pub enum Rules {
    /// Rules whose evidence names the folder itself.
    Precise,
    /// Rules that read an unnamed subtree below it.
    Subtree,
}

/// Engine questions for one scan, with each folder read at most once.
#[derive(Default)]
pub struct Detector {
    listings: RefCell<HashMap<PathBuf, Rc<Listing>>>,
    answers: RefCell<HashMap<(PathBuf, u8), Option<&'static str>>>,
}

const UNITY_PLAYERS: &[&str] = &["unityplayer.dll", "unityplayer.so", "unityplayer.dylib"];
const GODOT_PROGRAMS: &[&str] = &["exe", "x86_64", "x86", ""];

impl Detector {
    pub fn listing(&self, dir: &Path) -> Rc<Listing> {
        if let Some(l) = self.listings.borrow().get(dir) {
            return l.clone();
        }
        let l = Rc::new(Listing::read(dir));
        self.listings.borrow_mut().insert(dir.to_path_buf(), l.clone());
        l
    }

    /// The first row, in the database's order, with a rule of [which] kind
    /// that holds for [dir]. [at_only] asks a subtree rule about the folder
    /// itself (Unity's player beside its program, not three folders down).
    pub fn detect(&self, dir: &Path, which: Rules, at_only: bool) -> Option<&'static str> {
        let slot = match (which, at_only) {
            (Rules::Precise, _) => 0,
            (Rules::Subtree, false) => 1,
            (Rules::Subtree, true) => 2,
        };
        let key = (dir.to_path_buf(), slot);
        if let Some(answer) = self.answers.borrow().get(&key) {
            return *answer;
        }
        let listing = self.listing(dir);
        let answer = database()
            .iter()
            .find(|engine| {
                engine
                    .rules
                    .iter()
                    .any(|rule| (rule.subtree == (which == Rules::Subtree)) && rule.all.iter().all(|c| self.holds(c, &listing, at_only)))
            })
            .map(|engine| engine.id.as_str());
        self.answers.borrow_mut().insert(key, answer);
        answer
    }

    /// A relative path (slash separated) below [l] as it is on disk, each
    /// part matched without case.
    fn resolve(&self, l: &Listing, path: &str, want_dir: bool) -> Option<PathBuf> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let (last, folders) = parts.split_last()?;
        let mut held: Option<Rc<Listing>> = None;
        for part in folders {
            let here: &Listing = held.as_deref().unwrap_or(l);
            let next = here.dir.join(Listing::entry(&here.dirs, part)?);
            held = Some(self.listing(&next));
        }
        let here: &Listing = held.as_deref().unwrap_or(l);
        let names = if want_dir { &here.dirs } else { &here.files };
        Listing::entry(names, last).map(|n| here.dir.join(n))
    }

    fn holds(&self, condition: &Condition, l: &Listing, at_only: bool) -> bool {
        match condition {
            Condition::DirExists(path) => self.resolve(l, path, true).is_some(),
            Condition::FileExists(path) => self.resolve(l, path, false).is_some(),
            Condition::NameContains(value) => l.files.iter().any(|n| n.to_lowercase().contains(value.as_str())),
            Condition::Extension(value) => l.files.iter().any(|n| extension(n) == *value),
            Condition::ExtensionDeep(value, depth) => self.extension_within(l, value, *depth),
            Condition::NameIn(values) => l.files.iter().any(|n| values.contains(&n.to_lowercase())),
            Condition::DirPrefixCount(prefix, min) => {
                l.dirs.iter().filter(|n| n.to_lowercase().starts_with(prefix.as_str())).count() >= *min
            }
            Condition::FileHead(path, regex) => {
                self.resolve(l, path, false).and_then(|p| head(&p, 4096)).is_some_and(|h| regex.is_match(&latin1(&h)))
            }
            Condition::Builtin(name) => match name.as_str() {
                "godot" => self.godot(l),
                "html" => twine(l),
                "unity" => self.unity(l, if at_only { 0 } else { 3 }),
                // A probe this build does not ship fails its rule.
                _ => false,
            },
        }
    }

    fn extension_within(&self, l: &Listing, value: &str, depth: usize) -> bool {
        l.files.iter().any(|n| extension(n) == value)
            || (depth > 0 && l.dirs.iter().any(|d| self.extension_within(&self.listing(&l.dir.join(d)), value, depth - 1)))
    }

    fn unity(&self, l: &Listing, depth: usize) -> bool {
        l.files.iter().any(|n| UNITY_PLAYERS.contains(&n.to_lowercase().as_str()))
            || (depth > 0 && l.dirs.iter().any(|d| self.unity(&self.listing(&l.dir.join(d)), depth - 1)))
    }

    /// A loose `.pck`, or a program carrying Godot's pack at its end.
    fn godot(&self, l: &Listing) -> bool {
        l.files.iter().any(|n| extension(n) == "pck")
            || l.files.iter().filter(|n| GODOT_PROGRAMS.contains(&extension(n).as_str())).take(4).any(|n| embedded_pck(&l.dir.join(n)))
    }
}

/// Godot's embedded-pack export: the file ends with the pack's offset (u64,
/// little-endian) and `GDPC`. Only the last 12 bytes are read.
pub fn embedded_pck(file: &Path) -> bool {
    let Ok(mut f) = fs::File::open(file) else { return false };
    let Ok(size) = f.metadata().map(|m| m.len()) else { return false };
    if size < 12 || f.seek(SeekFrom::Start(size - 12)).is_err() {
        return false;
    }
    let mut tail = [0u8; 12];
    if f.read_exact(&mut tail).is_err() || &tail[8..] != b"GDPC" {
        return false;
    }
    let offset = u64::from_le_bytes(tail[..8].try_into().unwrap_or_default());
    offset > 0 && offset < size
}

/// A Twine story's page: a web page alone is no game (tools and launchers
/// ship `index.html` too). The page a game opens with is read first.
fn twine(l: &Listing) -> bool {
    let mut pages: Vec<&String> = l.files.iter().filter(|n| matches!(extension(n).as_str(), "html" | "htm")).collect();
    pages.sort_by_key(|n| !n.eq_ignore_ascii_case("index.html"));
    pages.into_iter().take(4).filter_map(|n| head(&l.dir.join(n), 1024 * 1024)).any(|bytes| {
        let text = latin1(&bytes).to_lowercase();
        text.contains("<tw-storydata") || text.chars().take(8 * 1024).collect::<String>().contains("story format")
    })
}

fn head(file: &Path, bytes: usize) -> Option<Vec<u8>> {
    let f = fs::File::open(file).ok()?;
    let mut out = Vec::new();
    f.take(bytes as u64).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Bytes as Latin-1 text: the markers the rules look for are ASCII.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dta-engines-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for f in files {
            let path = root.join(f);
            if f.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, b"x").unwrap();
            }
        }
        root
    }

    #[test]
    fn the_shipped_database_parses() {
        let ids: Vec<&str> = database().iter().map(|e| e.id.as_str()).collect();
        assert!(ids.len() > 40, "{ids:?}");
        assert_eq!(ids[0], "renpy");
        assert!(ids.contains(&"unity") && ids.contains(&"rpgmaker-mv") && ids.contains(&"kirikiri"));
    }

    #[test]
    fn engines_are_told_by_their_own_files() {
        let d = Detector::default();
        let renpy = tree("renpy", &["renpy/", "game/script.rpyc", "Game.exe"]);
        assert_eq!(d.detect(&renpy, Rules::Precise, false), Some("renpy"));
        let mv = tree("mv", &["www/js/rpg_core.js", "Game.exe"]);
        assert_eq!(d.detect(&mv, Rules::Precise, false), Some("rpgmaker-mv"));
        let kag = tree("kag", &["data.xp3", "Game.exe"]);
        assert_eq!(d.detect(&kag, Rules::Precise, false), Some("kirikiri"));
        let ags = tree("ags", &["acsetup.cfg", "Quest.ags"]);
        assert_eq!(d.detect(&ags, Rules::Precise, false), Some("ags"));
        let unreal = tree("unreal", &["Engine/Binaries/Win64/x.dll", "Game.exe"]);
        assert_eq!(d.detect(&unreal, Rules::Precise, false), Some("unreal"));
        let plain = tree("plain", &["Game.exe", "readme.txt"]);
        assert_eq!(d.detect(&plain, Rules::Precise, false), None);
        for root in [renpy, mv, kag, ags, unreal, plain] {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn subtree_rules_are_asked_apart() {
        let d = Detector::default();
        let unity = tree("unity", &["Wrapper/Game/UnityPlayer.dll", "Wrapper/Game/Game.exe"]);
        assert_eq!(d.detect(&unity, Rules::Precise, false), None);
        assert_eq!(d.detect(&unity, Rules::Subtree, false), Some("unity"));
        assert_eq!(d.detect(&unity, Rules::Subtree, true), None);
        assert_eq!(d.detect(&unity.join("Wrapper/Game"), Rules::Subtree, true), Some("unity"));
        let _ = fs::remove_dir_all(unity);
    }

    #[test]
    fn godot_packs_and_file_heads_are_read() {
        let root = tree("godot", &[]);
        fs::create_dir_all(&root).unwrap();
        let mut exe = vec![0u8; 64];
        exe.extend_from_slice(&16u64.to_le_bytes());
        exe.extend_from_slice(b"GDPC");
        fs::write(root.join("game.x86_64"), &exe).unwrap();
        assert!(embedded_pck(&root.join("game.x86_64")));
        let d = Detector::default();
        assert_eq!(d.detect(&root, Rules::Precise, false), Some("godot"));
        let vx = tree("vxace", &["Game.exe"]);
        fs::write(vx.join("Game.ini"), b"[Game]\r\nLibrary=System\\RGSS301.dll\r\n").unwrap();
        assert_eq!(d.detect(&vx, Rules::Precise, false), Some("rpgmaker-vxace"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(vx);
    }

    #[test]
    fn unknown_conditions_drop_their_rule() {
        let rows = parse(
            r#"{"engines":[{"id":"x","detect":[{"all":[{"type":"magic","value":"a"}]},{"all":[{"type":"anyFileExtension","value":"zz"}]}]}]}"#,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rules.len(), 1);
    }
}
