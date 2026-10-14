//! Game folders: the ones the person names (`droidtop-agent folders add
//! <path>`) and a `Games` folder at the top of a fixed drive (`~/Games`
//! elsewhere), walked with droidtop's own rules (droidtop SPEC 7g, 7i, 7m;
//! `GameEngineDetector.scan` and `PcFolderScan`), so the computer finds the
//! games the handheld would find in the same tree:
//!
//! - A folder with an engine's own layout (`engines`, the shared engines
//!   database) is a game, and so is a folder holding a program with no
//!   engine evidence (a PC game). Either stops the walk: a game's own
//!   folders are not further games.
//! - A folder holding two or more games is a collection, whatever it holds
//!   itself (`adult/godot`, with two games and a loose build beside them).
//! - A folder around a single game is that game when it is named after it
//!   or holds files of its own (a version folder, a payload folder);
//!   a folder with its own files and PC games below is the game they
//!   belong to (`Far Cry 5/bin`, `SimCity/SimCityRecovery`).
//! - A rule that only proves an engine game is somewhere below (Unity's
//!   player three folders down, compiled Ren'Py archives) names the
//!   outermost folder it matches, unless games sit below it.
//! - Folders named for a part or a version (`Week 2`, `12.0-scrappy`) cost
//!   no depth; anything else is walked at most four folders down.
//! - A Steam library met on the way is read from its `appmanifest` files,
//!   a ROM system folder (`gba`, `ps2`, with the system's own files) by the
//!   ROM rules, a Flashpoint install by `flashpoint`.
//!
//! A GOG or Steam id the game's own files state (`goggame-<id>.info`,
//! `steam_appid.txt`, its library's appmanifest) gives it that store's key,
//! so it meets the same game elsewhere; otherwise the key is its title,
//! read from the folder name without the version a download adds.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use droidtop_agent_core::library::title_key;
use regex::Regex;
use serde_json::Value;

use super::engines::{self, extension, Detector, Listing, Rules};
use super::{flashpoint, pc_game, roms, steam, Found};
use crate::state::Settings;

/// How many folders below a root the walk looks, not counting part and
/// version folders (droidtop's `MAX_SCAN_DEPTH`).
const MAX_DEPTH: usize = 4;

/// How many folders below a root a ROM system folder can sit (droidtop's
/// `MAX_SYSTEM_SEARCH_DEPTH`): `<root>/<system>` or `<root>/roms/<system>`.
const MAX_SYSTEM_DEPTH: usize = 2;

/// Folders that are never games (droidtop's `ScanPrune`).
const NEVER: &[&str] =
    &["system volume information", "$recycle.bin", "recycler", "lost+found", "found.000", "meta-inf", "__installer", "downloaded_media"];

// Programs that come with a game but are not it (droidtop's `PcFolderClassifier.roleOf`).
const UNINSTALLERS: &[&str] = &["unins"];
const INSTALLERS: &[&str] = &["setup", "install", "instmsi"];
const REDISTRIBUTABLES: &[&str] = &[
    "vcredist",
    "dxsetup",
    "dxwebsetup",
    "dotnet",
    "ndp",
    "netfx",
    "directx",
    "oalinst",
    "physx",
    "xnafx",
    "ue4prereq",
    "ueprereq",
    "windowsdesktopruntime",
];
const CRASH_HANDLERS: &[&str] = &[
    "crashpad",
    "crashreport",
    "crashhandler",
    "unitycrashhandler",
    "bugreport",
    "errorreport",
    "crashsender",
    "crashmonitor",
    "notificationhelper",
];

/// Whether a file starts a game: a program that is not an installer,
/// uninstaller, redistributable or crash handler. Settings screens and
/// patchers count, as droidtop counts them.
pub fn is_program(name: &str) -> bool {
    if !matches!(extension(name).as_str(), "exe" | "sh" | "x86_64" | "x86") {
        return false;
    }
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let base: String = stem.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    ![UNINSTALLERS, INSTALLERS, REDISTRIBUTABLES, CRASH_HANDLERS].iter().any(|list| list.iter().any(|p| base.starts_with(p)))
}

fn has_program(l: &Listing) -> bool {
    l.files.iter().any(|n| is_program(n)) || (cfg!(target_os = "macos") && l.dirs.iter().any(|d| d.to_lowercase().ends_with(".app")))
}

fn has_own_file(l: &Listing) -> bool {
    l.files.iter().any(|n| !n.starts_with('.'))
}

fn name_of(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn name_key(name: &str) -> String {
    name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a valid pattern"))
}

/// Folder names that are an engine's layout rather than a title
/// (droidtop's `EngineFolderNames`).
const ENGINE_FOLDERS: &[&str] = &[
    "game",
    "data",
    "www",
    "resources",
    "resource",
    "res",
    "lib",
    "lib64",
    "libs",
    "renpy",
    "contents",
    "assets",
    "bin",
    "binaries",
    "content",
    "engine",
    "runtime",
    "app",
    "src",
    "js",
    "win",
    "win32",
    "win64",
    "x86",
    "x64",
    "windows",
    "linux",
    "mac",
    "macos",
    "pc",
    "build",
    "builds",
    "release",
    "releases",
    "dist",
];

/// A folder named for an engine's layout (`game`, `content`, `win64_build`).
fn is_engine_folder(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    ENGINE_FOLDERS.contains(&lower.as_str()) || (words.len() > 1 && words.iter().all(|w| ENGINE_FOLDERS.contains(w)))
}

/// A folder named for a part or a version of one game (`Chap3+`, `Week 2`,
/// `Part II`, `12.0-scrappy`): droidtop's `GameNaming.isStructuralFolderName`.
pub fn is_structural(raw: &str) -> bool {
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    static PART: OnceLock<Regex> = OnceLock::new();
    static ROMAN: OnceLock<Regex> = OnceLock::new();
    static VERSION: OnceLock<Regex> = OnceLock::new();
    let name = regex(&BRACKETS, r"[\[({][^\])}]*[\])}]").replace_all(raw, " ").trim().to_string();
    if is_engine_folder(&name) {
        return false;
    }
    if regex(&VERSION, r"^\d+\.\d").is_match(&name) {
        return true;
    }
    let words = r"chap(?:ter)?|chp|part|week|act|episode|ep|season|vol(?:ume)?|day|disc|disk|book|bk|pt";
    let part = regex(&PART, &format!(r"(?i)^(?:{words})\.?\s*\W*\d"));
    let roman = regex(&ROMAN, &format!(r"^(?i:{words})\.?\s*[IVX]{{1,4}}[\s\-_+.]*$"));
    part.is_match(&name) || roman.is_match(&name)
}

/// A folder's name read as a title and a version, the way downloads name
/// them: `Eternum-0.9.5-pc` is Eternum 0.9.5, `LoveAndCorruption - v0.4.8
/// Public` is LoveAndCorruption 0.4.8, `Harem_Hotel-v0.20_Pre-Alpha-pc` is
/// Harem Hotel 0.20. A name with nothing to take off comes back as it is.
pub fn title_and_version(name: &str) -> (String, Option<String>) {
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    static MARKED: OnceLock<Regex> = OnceLock::new();
    static DOTTED: OnceLock<Regex> = OnceLock::new();
    let unbracketed = regex(&BRACKETS, r"\[[^\]]*\]|\([^)]*\)").replace_all(name, " ").into_owned();
    let mut s = strip_noise(&unbracketed);
    let mut version = None;
    let found = regex(&MARKED, r"(?i)(?:^|[-_ ])(?:v|ver\.?|version)\s*(\d+(?:[._]\d+)*[a-z0-9.]*)")
        .captures(&s)
        .or_else(|| regex(&DOTTED, r"(?i)[-_ ](\d+\.\d+(?:[._]\d+)*[a-z0-9.]*)").captures(&s));
    if let Some((all, v)) = found.and_then(|c| Some((c.get(0)?, c.get(1)?))) {
        version = Some(v.as_str().trim_end_matches('.').to_string());
        s = strip_noise(&s[..all.start()]);
    }
    if !s.contains(' ') {
        s = s.replace('_', " ");
    }
    if s.trim().is_empty() {
        return (name.to_string(), None);
    }
    (s.trim().to_string(), version)
}

/// [s] without the platform, store and edition words a download's name
/// ends with (`-pc`, `_win64`, ` Public`, `-F95ZONE`).
fn strip_noise(s: &str) -> String {
    static NOISE: OnceLock<Regex> = OnceLock::new();
    let noise = regex(
        &NOISE,
        r"(?i)[-_ .]+(pc|win|win32|win64|windows|linux|lin|mac|market|public|eng|en|uncensored|unc|deluxe|full|steam|f95zone|standard|animated|final)$",
    );
    let mut s = s.trim().to_string();
    loop {
        let next = noise.replace(&s, "").trim_matches(|c: char| c == ' ' || c == '-' || c == '_' || c == '.').to_string();
        if next == s {
            return s;
        }
        s = next;
    }
}

/// A game folder's title and version, with the part it is when its own
/// name is only a part or version (`Thief of Hearts/Part2`).
fn title_of(dir: &Path) -> (String, Option<String>) {
    static BARE_VERSION: OnceLock<Regex> = OnceLock::new();
    let mut parts = Vec::new();
    let mut version = None;
    let mut here = dir;
    while is_structural(&name_of(here)) || is_engine_folder(&name_of(here)) {
        let name = name_of(here);
        if regex(&BARE_VERSION, r"^\d+\.\d").is_match(&name) {
            version.get_or_insert(name);
        } else if !is_engine_folder(&name) {
            parts.push(name);
        }
        match here.parent() {
            Some(p) if !name_of(p).is_empty() => here = p,
            _ => break,
        }
    }
    let (title, own_version) = title_and_version(&name_of(here));
    parts.retain(|p| !name_key(&title).contains(&name_key(p)));
    parts.reverse();
    let title = std::iter::once(title).chain(parts).collect::<Vec<_>>().join(" ");
    (title, version.or(own_version))
}

/// The store key a game's own files state, when they state one. Of several
/// `goggame-<id>.info` files (the game's and its DLCs'), the game's own.
pub(super) fn store_key(dir: &Path) -> Option<String> {
    let mut gog = None;
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("goggame-") && name.ends_with(".info")) {
                continue;
            }
            let Some(info) = fs::read_to_string(entry.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
            let Some(id) = info["gameId"].as_str() else { continue };
            let root = info["rootGameId"].as_str().unwrap_or(id);
            if id == root {
                return Some(format!("gog:{id}"));
            }
            gog.get_or_insert(format!("gog:{root}"));
        }
    }
    if gog.is_some() {
        return gog;
    }
    let id = fs::read_to_string(dir.join("steam_appid.txt")).ok()?;
    let id = id.trim();
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| format!("steam:{id}"))
}

/// A store's own install root: its games are the store's business and its
/// other folders are no games (droidtop's `ScanPrune` store trees).
struct StoreTree {
    /// The folder's own name, when the store names it.
    name: Option<&'static str>,
    markers: &'static [&'static str],
    /// Where its games are, relative to the root.
    games: &'static str,
}

const STORE_TREES: &[StoreTree] = &[
    StoreTree { name: None, markers: &["steamapps", "libraryfolder.vdf", "libraryfolders.vdf"], games: "steamapps/common" },
    StoreTree { name: Some("gog galaxy"), markers: &["dependencies", "games"], games: "games" },
];

/// The ROM system a folder is named for and holds files of, if it is one.
fn rom_system(dir: &Path, l: &Listing, detector: &Detector) -> Option<&'static str> {
    let (id, extensions) = roms::system_named(&name_of(dir))?;
    let found = runs(l, extensions) || l.dirs.iter().any(|d| runs(&detector.listing(&dir.join(d)), extensions));
    found.then_some(id)
}

/// Whether a folder holds a file of one of [extensions].
fn runs(l: &Listing, extensions: &[String]) -> bool {
    l.files.iter().any(|f| extensions.contains(&extension(f)))
}

/// What the walk found under one folder.
enum Hit {
    /// A game folder: [display] is the folder the person sees as the game,
    /// [engine] what made it (none for a plain PC game), [precise] whether
    /// the evidence names this folder rather than something below it.
    Game { display: PathBuf, engine: Option<&'static str>, precise: bool, wrapped: bool },
    /// A game a store, a launcher or the ROM rules already describe.
    Ready(Box<Found>),
}

struct Walk {
    detector: Detector,
}

impl Walk {
    fn store_tree_at(&self, dir: &Path) -> Option<&'static StoreTree> {
        let name = name_of(dir).to_lowercase();
        let l = self.detector.listing(dir);
        let has = |marker: &str| l.dirs.iter().chain(l.files.iter()).any(|n| n.eq_ignore_ascii_case(marker));
        STORE_TREES.iter().find(|t| t.name.is_none_or(|n| n == name) && t.markers.iter().any(|m| has(m)))
    }

    /// The store tree [dir] is below, with [dir] relative to its root.
    fn store_tree_above(&self, dir: &Path) -> Option<(&'static StoreTree, String)> {
        let mut relative = name_of(dir);
        let mut parent = dir.parent();
        for _ in 0..6 {
            let p = parent.filter(|p| !name_of(p).is_empty())?;
            if let Some(tree) = self.store_tree_at(p) {
                return Some((tree, relative.to_lowercase()));
            }
            relative = format!("{}/{relative}", name_of(p));
            parent = p.parent();
        }
        None
    }

    fn inside_store_tree(&self, dir: &Path) -> bool {
        self.store_tree_at(dir).is_some() || self.store_tree_above(dir).is_some()
    }

    /// Folders that are never walked: hidden ones, the system's, a store's
    /// installer payload, scraped media, and a store tree's own folders
    /// away from where its games are.
    fn skipped(&self, dir: &Path) -> bool {
        let name = name_of(dir);
        if name.starts_with('.') || NEVER.contains(&name.to_lowercase().as_str()) {
            return true;
        }
        match self.store_tree_above(dir) {
            Some((tree, rel)) => {
                !(rel == tree.games || rel.starts_with(&format!("{}/", tree.games)) || tree.games.starts_with(&format!("{rel}/")))
            }
            None => false,
        }
    }

    fn engine_here(&self, dir: &Path, l: &Listing) -> Option<&'static str> {
        self.detector
            .detect(dir, Rules::Precise, false)
            .or_else(|| has_program(l).then(|| self.detector.detect(dir, Rules::Subtree, true)).flatten())
    }

    /// Two or more of the folder's own folders are engine games: it is a
    /// collection, whatever it holds itself.
    fn holds_games(&self, dir: &Path, l: &Listing) -> bool {
        let mut found = 0;
        for d in &l.dirs {
            let child = dir.join(d);
            if self.skipped(&child) || roms::system_named(d).is_some() {
                continue;
            }
            if self.engine_here(&child, &self.detector.listing(&child)).is_some() {
                found += 1;
                if found >= 2 {
                    return true;
                }
            }
        }
        false
    }

    fn visit(&self, dir: &Path, depth: usize) -> Vec<Hit> {
        let l = self.detector.listing(dir);
        if self.store_tree_at(dir).is_some_and(|t| t.name.is_none()) {
            return steam::library_games(dir).into_iter().map(|f| Hit::Ready(Box::new(f))).collect();
        }
        if let Some(found) = flashpoint::at(dir, &l) {
            return found.into_iter().map(|f| Hit::Ready(Box::new(f))).collect();
        }
        let store_root = self.store_tree_at(dir).is_some();
        let precise = self.engine_here(dir, &l);
        let mut holds = None;
        let mut holds_games = || *holds.get_or_insert_with(|| self.holds_games(dir, &l));
        if precise.is_some() && !store_root && !holds_games() {
            return vec![Hit::Game { display: dir.to_path_buf(), engine: precise, precise: true, wrapped: false }];
        }
        let subtree = self.detector.detect(dir, Rules::Subtree, false);
        if precise.is_none() && subtree.is_none() && has_program(&l) && !store_root && !holds_games() {
            return vec![Hit::Game { display: dir.to_path_buf(), engine: None, precise: true, wrapped: false }];
        }
        let mut below = Vec::new();
        for d in &l.dirs {
            let child = dir.join(d);
            if self.skipped(&child) {
                continue;
            }
            let child_depth = if is_structural(d) { depth } else { depth + 1 };
            if child_depth <= MAX_SYSTEM_DEPTH && !self.inside_store_tree(&child) {
                if let Some(system) = rom_system(&child, &self.detector.listing(&child), &self.detector) {
                    let mut roms = Vec::new();
                    roms::system_roms(system, &child, 0, &mut roms);
                    below.extend(roms.into_iter().map(|f| Hit::Ready(Box::new(f))));
                    continue;
                }
            }
            if child_depth <= MAX_DEPTH {
                below.extend(self.visit(&child, child_depth));
            }
        }
        let engine_games: Vec<&Hit> = below.iter().filter(|h| matches!(h, Hit::Game { engine: Some(_), .. })).collect();
        let pc_games = below.iter().filter(|h| matches!(h, Hit::Game { engine: None, .. })).count();
        let own_files = has_own_file(&l);
        // Files of its own and PC games below: this folder is the game they belong to.
        if pc_games > 0
            && engine_games.is_empty()
            && own_files
            && !is_structural(&name_of(dir))
            && !self.inside_store_tree(dir)
            && !holds_games()
        {
            below.retain(|h| matches!(h, Hit::Ready(_)));
            below.push(Hit::Game { display: dir.to_path_buf(), engine: None, precise: true, wrapped: false });
            return below;
        }
        // One game directly inside, named after this folder or beside its files: this folder is the game.
        if let [Hit::Game { display, engine: Some(engine), precise: true, wrapped: false }] = engine_games.as_slice() {
            let named_after = name_key(&name_of(display)).starts_with(&name_key(&name_of(dir)));
            if display.parent() == Some(dir) && (named_after || (own_files && !self.inside_store_tree(dir))) {
                let engine = *engine;
                below.retain(|h| !matches!(h, Hit::Game { engine: Some(_), .. }));
                below.push(Hit::Game { display: dir.to_path_buf(), engine: Some(engine), precise: true, wrapped: true });
                return below;
            }
        }
        // Only a rule reading below names this folder: the outermost folder it matches is the game.
        if let Some(engine) = subtree {
            let precise_below = engine_games.iter().any(|h| matches!(h, Hit::Game { precise: true, .. }));
            if !store_root && engine_games.len() <= 1 && !precise_below {
                below.retain(|h| !matches!(h, Hit::Game { engine: Some(_), .. }));
                below.push(Hit::Game { display: dir.to_path_buf(), engine: Some(engine), precise: false, wrapped: false });
                return below;
            }
        }
        // A collection: a loose program with Godot's pack inside is a game of its own.
        for f in &l.files {
            if is_program(f) && engines::embedded_pck(&dir.join(f)) {
                below.push(Hit::Ready(Box::new(found(&dir.join(f), Some("godot")))));
            }
        }
        below
    }
}

/// A game folder (or a single-file game) as the library's entry.
fn found(path: &Path, engine: Option<&'static str>) -> Found {
    let (title, version) = if path.is_file() {
        title_and_version(path.file_stem().map(|s| s.to_string_lossy()).as_deref().unwrap_or_default())
    } else {
        title_of(path)
    };
    let key = store_key(path).or_else(|| steam::installed_key(path)).unwrap_or_else(|| title_key(&title));
    let mut f = pc_game(key, title, Some(path.to_path_buf()), "folder", 0, version);
    f.engine = engine.map(str::to_string);
    f
}

/// The game folders a scan walks: the person's, and a `Games` folder at
/// the top of each fixed drive (`~/Games` elsewhere). A folder inside
/// another is walked once, as part of the outer one.
pub fn roots(settings: &Settings) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = settings.game_folders.clone();
    for found in discovered() {
        if !all.iter().any(|r| same_path(r, &found)) {
            all.push(found);
        }
    }
    let canonical: Vec<PathBuf> = all.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect();
    let outermost: Vec<bool> = canonical.iter().map(|c| !canonical.iter().any(|o| o != c && c.starts_with(o))).collect();
    all.into_iter().zip(outermost).filter_map(|(p, keep)| keep.then_some(p)).collect()
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(windows)]
fn discovered() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    // DRIVE_FIXED: a removable or network drive is never walked on its own.
    const DRIVE_FIXED: u32 = 3;
    // SAFETY: no arguments; returns a bit mask of the drive letters in use.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1u32 << i) != 0)
        .map(|i| format!("{}:\\", (b'A' + i) as char))
        .filter(|root| {
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: a NUL-terminated wide string that outlives the call.
            unsafe { GetDriveTypeW(wide.as_ptr()) == DRIVE_FIXED }
        })
        .map(|root| PathBuf::from(root).join("Games"))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(not(windows))]
fn discovered() -> Vec<PathBuf> {
    [super::home().join("Games")].into_iter().filter(|p| p.is_dir()).collect()
}

/// Every game under one root.
pub fn walk(root: &Path) -> Vec<Found> {
    let walk = Walk { detector: Detector::default() };
    let mut out = Vec::new();
    let l = walk.detector.listing(root);
    if walk.store_tree_at(root).is_some_and(|t| t.name.is_none()) {
        return steam::library_games(root);
    }
    for d in &l.dirs {
        let child = root.join(d);
        if walk.skipped(&child) {
            continue;
        }
        if let Some(system) = rom_system(&child, &walk.detector.listing(&child), &walk.detector) {
            roms::system_roms(system, &child, 0, &mut out);
            continue;
        }
        for hit in walk.visit(&child, 1) {
            match hit {
                Hit::Game { display, engine, .. } => out.push(found(&display, engine)),
                Hit::Ready(f) => out.push(*f),
            }
        }
    }
    out
}

pub fn scan(settings: &Settings) -> Result<Vec<Found>, String> {
    let mut out = Vec::new();
    for root in roots(settings) {
        out.extend(walk(&root));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture tree: paths ending in `/` are folders, others files.
    fn tree(name: &str, entries: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dta-folders-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for e in entries {
            let path = root.join(e);
            if e.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, b"x").unwrap();
            }
        }
        root
    }

    /// What the walk finds: each entry's path below the root, key and engine.
    fn found_in(root: &Path) -> Vec<(String, String, Option<String>)> {
        let mut out: Vec<(String, String, Option<String>)> = walk(root)
            .into_iter()
            .map(|f| {
                let path = PathBuf::from(f.game.install.path.clone().unwrap_or_default());
                let rel = path.strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                (rel, f.game.key, f.engine)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn programs_and_helpers_are_told_apart() {
        assert!(is_program("Game.exe"));
        assert!(is_program("game.x86_64"));
        assert!(is_program("Config.exe"));
        assert!(!is_program("unins000.exe"));
        assert!(!is_program("UnityCrashHandler64.exe"));
        assert!(!is_program("vcredist_x64.exe"));
        assert!(!is_program("readme.txt"));
    }

    #[test]
    fn titles_lose_the_version_a_download_adds() {
        assert_eq!(title_and_version("Eternum-0.9.5-pc"), ("Eternum".into(), Some("0.9.5".into())));
        assert_eq!(title_and_version("LoveAndCorruption - v0.4.8 Public"), ("LoveAndCorruption".into(), Some("0.4.8".into())));
        assert_eq!(title_and_version("Harem_Hotel-v0.20_Pre-Alpha-pc"), ("Harem Hotel".into(), Some("0.20".into())));
        assert_eq!(title_and_version("Chubby_Story_Windows_1.15.0"), ("Chubby Story".into(), Some("1.15.0".into())));
        assert_eq!(title_and_version("Robolife 2 - Nova Duty v1.71"), ("Robolife 2 - Nova Duty".into(), Some("1.71".into())));
        assert_eq!(title_and_version("Stardew Valley"), ("Stardew Valley".into(), None));
        assert_eq!(title_and_version("Life No.0"), ("Life No.0".into(), None));
        assert_eq!(title_and_version("Magic & Slash (MangaGamer)"), ("Magic & Slash".into(), None));
    }

    #[test]
    fn parts_and_versions_are_structure_not_titles() {
        assert!(is_structural("Week 2"));
        assert!(is_structural("Part1"));
        assert!(is_structural("Chap3+"));
        assert!(is_structural("12.0-scrappy"));
        assert!(is_structural("Act II"));
        assert!(!is_structural("Thief of Hearts"));
        assert!(!is_structural("game"));
        assert!(!is_structural("Day One Something"));
    }

    #[test]
    fn engine_games_collections_and_wrappers() {
        let root = tree(
            "engines",
            &[
                // A category folder of engine games.
                "renpy/Eternum-0.9.5-pc/renpy/",
                "renpy/Eternum-0.9.5-pc/game/script.rpyc",
                "renpy/Eternum-0.9.5-pc/Eternum.exe",
                // A version folder around a game.
                "renpy/Astra/Astra Show 1.1/renpy/",
                "renpy/Astra/Astra Show 1.1/game/a.rpa",
                // Parts of one game.
                "renpy/Thief of Hearts/Part1/renpy/",
                "renpy/Thief of Hearts/Part1/game/x.rpyc",
                "renpy/Thief of Hearts/Part2/renpy/",
                "renpy/Thief of Hearts/Part2/game/x.rpyc",
                // Unity, its player beside the program, and one packaged a few folders down.
                "unity/Clover Days/Clover Days.exe",
                "unity/Clover Days/UnityPlayer.dll",
                "unity/Clover Days/Clover Days_Data/",
                "unity/Love Book3/content/win64_build/book3/UnityPlayer.dll",
                "unity/Love Book3/content/win64_build/book3/book3.exe",
                // RPG Maker MV.
                "rpgm/Lily/www/js/rpg_core.js",
                "rpgm/Lily/Game.exe",
                // AGS, in a folder named like ES-DE's ags system.
                "ags/Heroine's Quest/acsetup.cfg",
                "ags/Heroine's Quest/Heroine's Quest.ags",
                "ags/5 Days a Stranger/5days.exe",
                // An empty shell is no game.
                "unity/Gone_1.2.3/.gamenative",
            ],
        );
        let got = found_in(&root);
        let keys: Vec<(&str, &str, Option<&str>)> = got.iter().map(|(p, k, e)| (p.as_str(), k.as_str(), e.as_deref())).collect();
        assert_eq!(
            keys,
            vec![
                ("ags/5 Days a Stranger", "title:5 days a stranger", None),
                ("ags/Heroine's Quest", "title:heroine s quest", Some("ags")),
                ("renpy/Astra", "title:astra", Some("renpy")),
                ("renpy/Eternum-0.9.5-pc", "title:eternum", Some("renpy")),
                ("renpy/Thief of Hearts/Part1", "title:thief of hearts part1", Some("renpy")),
                ("renpy/Thief of Hearts/Part2", "title:thief of hearts part2", Some("renpy")),
                ("rpgm/Lily", "title:lily", Some("rpgmaker-mv")),
                ("unity/Clover Days", "title:clover days", Some("unity")),
                ("unity/Love Book3/content/win64_build/book3", "title:love book3", Some("unity")),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pc_games_stores_and_roms() {
        let root = tree(
            "pc",
            &[
                // A PC game whose programs sit in sub-folders, with files of its own.
                "Ubisoft/Far Cry 5/uplay_install.state",
                "Ubisoft/Far Cry 5/bin/FarCry5.exe",
                "Ubisoft/Far Cry 5/bin_plus/FarCry5.exe",
                // A store's installer payload is no game.
                "EA/SimCity/__Installer/Touchup.exe",
                "EA/SimCity/SimCity/SimCity.exe",
                "EA/SimCity/SimCityRecovery/SimCityRecovery.exe",
                "EA/SimCity/core.dat",
                // A GOG game with a DLC's info file beside its own.
                "GOG/Cyberpunk 2077/goggame-1256837418.info",
                "GOG/Cyberpunk 2077/goggame-1423049311.info",
                "GOG/Cyberpunk 2077/REDprelauncher.exe",
                // A Steam library.
                "Steam/steamapps/common/Big Game/bin/big.exe",
                "Steam/steamapps/common/Leftover/cfg.txt",
                "Steam/steamapps/workshop/content/1/x.exe",
                // ROMs, one container level down.
                "roms/gba/Pokemon - LeafGreen (USA).gba",
                "roms/ps2/Kingdom Hearts (USA)/Kingdom Hearts (USA).iso",
            ],
        );
        fs::write(
            root.join("GOG/Cyberpunk 2077/goggame-1256837418.info"),
            br#"{"gameId":"1256837418","rootGameId":"1423049311","name":"Phantom Liberty"}"#,
        )
        .unwrap();
        fs::write(
            root.join("GOG/Cyberpunk 2077/goggame-1423049311.info"),
            br#"{"gameId":"1423049311","rootGameId":"1423049311","name":"Cyberpunk 2077"}"#,
        )
        .unwrap();
        fs::write(
            root.join("Steam/steamapps/appmanifest_42.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"42\"\n\t\"name\"\t\t\"Big Game\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"Big Game\"\n}\n",
        )
        .unwrap();
        let got = found_in(&root);
        let keys: Vec<(&str, &str)> = got.iter().map(|(p, k, _)| (p.as_str(), k.as_str())).collect();
        assert_eq!(
            keys,
            vec![
                ("EA/SimCity", "title:simcity"),
                ("GOG/Cyberpunk 2077", "gog:1423049311"),
                ("Steam/steamapps/common/Big Game", "steam:42"),
                ("Ubisoft/Far Cry 5", "title:far cry 5"),
                ("roms/gba/Pokemon - LeafGreen (USA).gba", "rom:gba/pokemon leafgreen usa"),
                ("roms/ps2/Kingdom Hearts (USA)/Kingdom Hearts (USA).iso", "rom:ps2/kingdom hearts usa"),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_collection_with_a_loose_build_keeps_its_games() {
        let root = tree(
            "godot",
            &[
                "godot/Goodbye Eternity/game.pck",
                "godot/Goodbye Eternity/Goodbye.exe",
                "godot/Coffee_v1.2-windows/Coffee.pck",
                "godot/Coffee_v1.2-windows/Coffee.exe",
            ],
        );
        let mut exe = vec![0u8; 64];
        exe.extend_from_slice(&16u64.to_le_bytes());
        exe.extend_from_slice(b"GDPC");
        fs::write(root.join("godot/Coffee-1.0-linux.x86_64"), &exe).unwrap();
        let got = found_in(&root);
        let keys: Vec<(&str, &str)> = got.iter().map(|(p, k, _)| (p.as_str(), k.as_str())).collect();
        assert_eq!(
            keys,
            vec![
                ("godot/Coffee-1.0-linux.x86_64", "title:coffee"),
                ("godot/Coffee_v1.2-windows", "title:coffee"),
                ("godot/Goodbye Eternity", "title:goodbye eternity")
            ]
        );
        let _ = fs::remove_dir_all(root);
    }
}
