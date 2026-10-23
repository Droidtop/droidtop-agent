//! Where a game keeps its saves by its engine's own rule, without the
//! Ludusavi manifest (Droidtop/tracker#538, the owner's Windows test): an
//! engine that decides its save folder the same way for every game it runs
//! is asked first, the way Enginehost knows each engine's own save logic
//! (droidtop docs/SPEC.md, Enginehost "Saves").
//!
//! - **Ren'Py** keeps every save twice and merges the two itself, newest
//!   wins: beside the game in `game/saves`, and in the person's folder under
//!   the game's `config.save_directory` (`%APPDATA%\RenPy\<dir>` on Windows,
//!   `~/.renpy/<dir>` on Linux, `~/Library/RenPy/<dir>` on macOS). The name
//!   is read from the game's scripts: `options.rpy`, or the compiled
//!   `options.rpyc` when only that ships; failing both, a folder in the
//!   person's Ren'Py folder named for the game (`<Title>-<digits>`).

use std::fs;
use std::io::Read;
use std::path::Path;

use droidtop_agent_core::saves::{Roots, SaveSpec};

/// The engines-database id of Ren'Py.
pub const RENPY: &str = "renpy";

/// Whether the engine keeps one set of saves in several places that it
/// merges itself, so syncing any one of them carries all of them.
pub fn mirrors(engine: Option<&str>) -> bool {
    engine == Some(RENPY)
}

/// The engine's own rule for a game in [`base`], or None when the engine has
/// none (most engines leave it to each game).
pub fn spec_for(engine: &str, base: &Path, title: &str, roots: &Roots) -> Option<SaveSpec> {
    match engine {
        RENPY => Some(renpy(base, title, roots)),
        _ => None,
    }
}

/// The folders under the person's home where Ren'Py keeps a game's second
/// copy, as save templates: one per system, so each side resolves its own.
const RENPY_HOMES: &[&str] = &["<winAppData>/RenPy", "<home>/.renpy", "<home>/Library/RenPy"];

fn renpy(base: &Path, title: &str, roots: &Roots) -> SaveSpec {
    let mut patterns = vec!["<base>/game/saves".to_string()];
    let dir = renpy_save_directory(base).or_else(|| renpy_folder_for(title, roots));
    if let Some(dir) = dir.filter(|d| safe_name(d)) {
        patterns.extend(RENPY_HOMES.iter().map(|home| format!("{home}/{dir}")));
    }
    SaveSpec { patterns }
}

/// A save directory name that stays one folder: no separators or climbing.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

/// `config.save_directory` as the game's scripts set it.
pub fn renpy_save_directory(base: &Path) -> Option<String> {
    let game = base.join("game");
    for name in ["options.rpy", "script.rpy"] {
        if let Ok(text) = fs::read_to_string(game.join(name)) {
            if let Some(dir) = save_directory_in_source(&text) {
                return Some(dir);
            }
        }
    }
    let compiled = fs::read(game.join("options.rpyc")).ok()?;
    save_directory_in_compiled(&rpyc_data(&compiled)?)
}

/// `define config.save_directory = "Name-1234"` (or the older
/// `config.save_directory = ...` in an init block) in a script's source.
fn save_directory_in_source(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let at = line.find("config.save_directory")?;
        let rest = line[at + "config.save_directory".len()..].trim_start().strip_prefix('=')?;
        quoted(rest.trim_start())
    })
}

/// The first string in quotes at the start of [`text`].
fn quoted(text: &str) -> Option<String> {
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &text[1..];
    rest.find(quote).map(|end| rest[..end].to_string())
}

/// A compiled script's pickled tree, unpacked: `RENPY RPC2` files hold
/// slots (number, start, length) after the header, slot 1 being the tree;
/// older ones are the zlib stream alone.
fn rpyc_data(bytes: &[u8]) -> Option<Vec<u8>> {
    const HEADER: &[u8] = b"RENPY RPC2";
    let inflate = |data: &[u8]| {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(data).take(64 << 20).read_to_end(&mut out).ok().map(|_| out)
    };
    if !bytes.starts_with(HEADER) {
        return inflate(bytes);
    }
    let mut at = HEADER.len();
    while at + 12 <= bytes.len() {
        let word = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        let (slot, start, length) = (word(at), word(at + 4), word(at + 8));
        if slot == 0 {
            break;
        }
        if slot == 1 {
            return inflate(bytes.get(start..start.checked_add(length)?)?);
        }
        at += 12;
    }
    None
}

/// The save directory in a compiled script: the pickle keeps the statement's
/// name and its value's source text close together, so the first quoted
/// string after `save_directory` is the value.
fn save_directory_in_compiled(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    let mut from = 0;
    while let Some(at) = text[from..].find("save_directory") {
        let after = from + at + "save_directory".len();
        let window: String = text[after..].chars().take(400).collect();
        for quote in ['"', '\''] {
            if let Some(start) = window.find(quote) {
                let rest = &window[start + 1..];
                if let Some(end) = rest.find(quote) {
                    let value = &rest[..end];
                    if safe_name(value) && value.len() <= 120 && !value.contains(['\n', '\r']) {
                        return Some(value.to_string());
                    }
                }
            }
        }
        from = after;
    }
    None
}

/// A folder in the person's Ren'Py folder named for the game, as Ren'Py's
/// own default names it (`<Title>-<digits>`), when the scripts do not say.
fn renpy_folder_for(title: &str, roots: &Roots) -> Option<String> {
    let want = squash(title);
    if want.is_empty() {
        return None;
    }
    let homes = [roots.get("<winAppData>").map(|r| r.join("RenPy")), roots.get("<home>").map(|h| h.join(".renpy"))];
    for home in homes.into_iter().flatten() {
        let Ok(dir) = fs::read_dir(&home) else { continue };
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((stem, digits)) = name.rsplit_once('-') else { continue };
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) && squash(stem) == want {
                return Some(name);
            }
        }
    }
    None
}

/// Letters and digits only, lower case: "Being a DIK" and "BeingADIK" agree.
fn squash(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dta-engines-saves-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("game")).unwrap();
        dir
    }

    #[test]
    fn options_rpy_names_the_save_directory() {
        let text = "# config.save_directory = \"Not-this\"\ndefine config.save_directory = \"Moonlit-1700000000\"\n";
        assert_eq!(save_directory_in_source(text).as_deref(), Some("Moonlit-1700000000"));
        assert_eq!(save_directory_in_source("    $ config.save_directory = 'Old-1'").as_deref(), Some("Old-1"));
        assert_eq!(save_directory_in_source("define config.name = \"x\""), None);
    }

    #[test]
    fn a_compiled_options_file_is_read_too() {
        let base = temp("rpyc");
        let tree = b"\x80\x02crenpy.ast\nDefine\nq\x00X\x0e\x00\x00\x00save_directoryq\x01X\x06\x00\x00\x00configX\x14\x00\x00\x00\"Lantern-1650000000\"";
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zlib.write_all(tree).unwrap();
        let packed = zlib.finish().unwrap();
        let start = 10 + 12 * 2;
        let mut file = b"RENPY RPC2".to_vec();
        for word in [1u32, start as u32, packed.len() as u32, 0, 0, 0] {
            file.extend(word.to_le_bytes());
        }
        file.extend(&packed);
        fs::write(base.join("game/options.rpyc"), &file).unwrap();
        assert_eq!(renpy_save_directory(&base).as_deref(), Some("Lantern-1650000000"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn renpy_saves_are_beside_the_game_and_in_the_persons_folder() {
        let base = temp("spec");
        fs::write(base.join("game/options.rpy"), "define config.save_directory = \"Moonlit-17\"\n").unwrap();
        let spec = spec_for(RENPY, &base, "Moonlit Harbor", &Roots::new()).unwrap();
        assert_eq!(
            spec.patterns,
            ["<base>/game/saves", "<winAppData>/RenPy/Moonlit-17", "<home>/.renpy/Moonlit-17", "<home>/Library/RenPy/Moonlit-17"]
        );
        assert!(spec_for("unity", &base, "x", &Roots::new()).is_none());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn without_scripts_the_folder_named_for_the_game_is_found() {
        let base = temp("guess");
        let appdata = temp("appdata");
        fs::create_dir_all(appdata.join("RenPy/BeingADIK-1580000000")).unwrap();
        fs::create_dir_all(appdata.join("RenPy/Other-1")).unwrap();
        let roots = Roots::from([("<winAppData>".to_string(), appdata.clone())]);
        let spec = spec_for(RENPY, &base, "Being a DIK", &roots).unwrap();
        assert!(spec.patterns.contains(&"<winAppData>/RenPy/BeingADIK-1580000000".to_string()));
        // Nothing to go on: the game's own folder is still known.
        assert_eq!(spec_for(RENPY, &base, "Unheard Of", &roots).unwrap().patterns, ["<base>/game/saves"]);
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&appdata);
    }
}
