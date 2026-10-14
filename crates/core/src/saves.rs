//! Where a game keeps its saves (docs/DESIGN.md section 6): templates in
//! Ludusavi's vocabulary, a root token then a path that may hold globs
//! (`<winAppData>/Game/*.sav`). Each side resolves the tokens in its own copy
//! of the game ([`Roots`]); files travel under canonical names, the token and
//! the path below it with forward slashes, compared without case.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The root tokens a save template may start with.
pub const TOKENS: &[&str] = &[
    "<base>",
    "<home>",
    "<winAppData>",
    "<winLocalAppData>",
    "<winLocalAppDataLow>",
    "<winDocuments>",
    "<winPublic>",
    "<winProgramData>",
    "<winDir>",
    "<xdgData>",
    "<xdgConfig>",
];

/// How deep a folder named by a template is walked.
const MAX_DEPTH: usize = 12;

/// A game's save locations, as the computer states them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct SaveSpec {
    pub patterns: Vec<String>,
}

/// A token's folder on this device.
pub type Roots = BTreeMap<String, PathBuf>;

/// A template split into its token and path components, or None when it does
/// not start with a known token or climbs out of it.
pub fn split(pattern: &str) -> Option<(&'static str, Vec<String>)> {
    let pattern = pattern.replace('\\', "/");
    let token: &'static str = TOKENS.iter().copied().find(|t| pattern.starts_with(t))?;
    let rest = &pattern[token.len()..];
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let parts: Vec<String> = rest.split('/').filter(|p| !p.is_empty() && *p != ".").map(str::to_string).collect();
    if parts.iter().any(|p| p == "..") {
        return None;
    }
    Some((token, parts))
}

/// Whether a component is safe to create on any of the three systems.
fn safe_component(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains(['/', '\\', ':', '\0'])
}

/// A canonical name's token and components, or None when it is malformed or
/// could leave its root.
pub fn parse_name(name: &str) -> Option<(&'static str, Vec<&str>)> {
    let token: &'static str = TOKENS.iter().copied().find(|t| name.starts_with(t))?;
    let rest = name[token.len()..].strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    parts.iter().all(|p| safe_component(p)).then_some((token, parts))
}

impl SaveSpec {
    /// Whether a canonical file name falls under one of the templates: a
    /// peer's file outside them is never written.
    pub fn matches(&self, name: &str) -> bool {
        let Some((token, parts)) = parse_name(name) else { return false };
        self.patterns.iter().any(|pattern| match split(pattern) {
            Some((t, pattern_parts)) if t == token => matches_parts(&pattern_parts, &parts),
            _ => false,
        })
    }
}

fn matches_parts(pattern: &[String], name: &[&str]) -> bool {
    match pattern.split_first() {
        // The template named a folder: everything under it is in.
        None => true,
        Some((first, rest)) if first == "**" => (0..=name.len()).any(|skip| matches_parts(rest, &name[skip..])),
        Some((first, rest)) => match name.split_first() {
            Some((part, name_rest)) => glob(first, part) && matches_parts(rest, name_rest),
            None => false,
        },
    }
}

/// `*` and `?` against one name, without case.
pub fn glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn has_glob(part: &str) -> bool {
    part.contains(['*', '?'])
}

/// The entries of a folder, skipping links: a save set never follows one.
fn entries(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        out.push((name, entry.path(), kind.is_dir()));
    }
    out.sort();
    out
}

/// Every save file on this device: canonical name and path.
pub fn collect(spec: &SaveSpec, roots: &Roots) -> Vec<(String, PathBuf)> {
    let mut found: BTreeMap<String, (String, PathBuf)> = BTreeMap::new();
    for pattern in &spec.patterns {
        let Some((token, parts)) = split(pattern) else { continue };
        let Some(root) = roots.get(token) else { continue };
        if !root.is_dir() {
            continue;
        }
        walk(root, &parts, token.to_string(), 0, &mut found);
    }
    found.into_values().collect()
}

fn walk(dir: &Path, parts: &[String], name: String, depth: usize, found: &mut BTreeMap<String, (String, PathBuf)>) {
    if depth > MAX_DEPTH {
        return;
    }
    match parts.split_first() {
        None => everything_under(dir, name, depth, found),
        Some((first, rest)) if first == "**" => {
            walk(dir, rest, name.clone(), depth, found);
            for (child, path, is_dir) in entries(dir) {
                if is_dir {
                    walk(&path, parts, format!("{name}/{child}"), depth + 1, found);
                }
            }
        }
        Some((first, rest)) => {
            for (child, path, is_dir) in entries(dir) {
                let hit = if has_glob(first) { glob(first, &child) } else { child.to_lowercase() == first.to_lowercase() };
                if !hit || !safe_component(&child) || child.ends_with(PART_SUFFIX) {
                    continue;
                }
                let child_name = format!("{name}/{child}");
                if is_dir {
                    walk(&path, rest, child_name, depth + 1, found);
                } else if rest.is_empty() {
                    found.entry(child_name.to_lowercase()).or_insert((child_name, path));
                }
            }
        }
    }
}

fn everything_under(dir: &Path, name: String, depth: usize, found: &mut BTreeMap<String, (String, PathBuf)>) {
    if depth > MAX_DEPTH {
        return;
    }
    for (child, path, is_dir) in entries(dir) {
        if !safe_component(&child) || child.ends_with(PART_SUFFIX) {
            continue;
        }
        let child_name = format!("{name}/{child}");
        if is_dir {
            everything_under(&path, child_name, depth + 1, found);
        } else {
            found.entry(child_name.to_lowercase()).or_insert((child_name, path));
        }
    }
}

/// The Windows roots inside a Wine or Proton prefix: [`drive_c`] is the
/// prefix's `drive_c`, [`user`] the Windows user in it. The same layout
/// droidtop's Steam Cloud sync maps (`SaveLayout.windowsDirs`), used on the
/// handheld for its Wine prefixes and on a Linux computer for Proton's.
pub fn prefix_roots(drive_c: &Path, user: &str) -> Roots {
    let home = drive_c.join("users").join(user);
    let mut r = Roots::new();
    r.insert("<home>".into(), home.clone());
    r.insert("<winAppData>".into(), home.join("AppData/Roaming"));
    r.insert("<winLocalAppData>".into(), home.join("AppData/Local"));
    r.insert("<winLocalAppDataLow>".into(), home.join("AppData/LocalLow"));
    r.insert("<winDocuments>".into(), home.join("Documents"));
    r.insert("<winPublic>".into(), drive_c.join("users/Public"));
    r.insert("<winProgramData>".into(), drive_c.join("ProgramData"));
    r.insert("<winDir>".into(), drive_c.join("windows"));
    r
}

/// The suffix of a file being written, never part of a save set.
pub const PART_SUFFIX: &str = ".dtpart";

/// Where a canonical name lives on this device: existing components are
/// matched without case, missing ones are created as named. None when the
/// name is malformed or its root is not here.
pub fn local_path(name: &str, roots: &Roots) -> Option<PathBuf> {
    let (token, parts) = parse_name(name)?;
    let mut path = roots.get(token)?.clone();
    for part in parts {
        let existing = entries(&path).into_iter().find(|(child, _, _)| child.to_lowercase() == part.to_lowercase());
        path = match existing {
            Some((_, p, _)) => p,
            None => path.join(part),
        };
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_without_case() {
        assert!(glob("*.sav", "Slot1.SAV"));
        assert!(glob("save?.dat", "save3.dat"));
        assert!(!glob("save?.dat", "save10.dat"));
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "aXXbYYc"));
    }

    #[test]
    fn templates_split_and_refuse_escapes() {
        assert_eq!(split("<winAppData>/Game/*.sav").unwrap(), ("<winAppData>", vec!["Game".to_string(), "*.sav".to_string()]));
        assert!(split("<winAppData>/../x").is_none());
        assert!(split("C:/Games/x").is_none());
        assert!(split("<winAppDataX>/x").is_none());
    }

    #[test]
    fn names_outside_the_spec_are_refused() {
        let spec = SaveSpec { patterns: vec!["<winAppData>/Game".into(), "<base>/saves/*.sav".into()] };
        assert!(spec.matches("<winAppData>/Game/slot1/data.bin"));
        assert!(spec.matches("<base>/saves/one.SAV"));
        assert!(!spec.matches("<base>/game.exe"));
        assert!(!spec.matches("<base>/saves/../game.exe"));
        assert!(!spec.matches("<winAppData>/Other/x"));
    }

    #[test]
    fn collect_finds_the_files_and_local_path_finds_them_again() {
        let dir = std::env::temp_dir().join(format!("dta-saves-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Roaming/Game/slots")).unwrap();
        fs::write(dir.join("Roaming/Game/slots/a.sav"), b"a").unwrap();
        fs::write(dir.join("Roaming/Game/config.ini"), b"c").unwrap();
        fs::write(dir.join("Roaming/Game/x.dtpart"), b"partial").unwrap();
        let mut roots = Roots::new();
        roots.insert("<winAppData>".into(), dir.join("Roaming"));
        let spec = SaveSpec { patterns: vec!["<winAppData>/game".into()] };
        let names: Vec<String> = collect(&spec, &roots).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["<winAppData>/Game/config.ini", "<winAppData>/Game/slots/a.sav"]);
        assert_eq!(local_path("<winAppData>/GAME/slots/a.sav", &roots).unwrap(), dir.join("Roaming/Game/slots/a.sav"));
        assert_eq!(local_path("<winAppData>/Game/new/b.sav", &roots).unwrap(), dir.join("Roaming/Game/new/b.sav"));
        assert!(local_path("<winAppData>/Game/../../etc", &roots).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
