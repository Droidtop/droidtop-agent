//! ROM folders: folders laid out by ES-DE system name (`snes/`, `psx/`...),
//! the naming droidtop uses for platforms (SPEC 7b). The person names them
//! (`droidtop-agent folders add --roms <path>`), and ES-DE's own ROM folder is
//! read from its settings when ES-DE is on this computer. A ROM's key is its
//! system and its file name without the extension, so the same game meets
//! across devices whatever its format.

use std::fs;
use std::path::{Path, PathBuf};

use droidtop_agent_core::library::{title_key, Install, ScannedGame};

use super::{home, Found};
use crate::state::Settings;

/// File types that are games in a system folder; artwork, saves and notes are not.
const ROM_TYPES: &[&str] = &[
    "zip", "7z", "chd", "iso", "cue", "m3u", "rvz", "wbfs", "gcz", "wad", "nsp", "xci", "3ds", "cia", "cci", "nds", "gba", "gbc", "gb",
    "sfc", "smc", "nes", "fds", "n64", "z64", "v64", "md", "gen", "smd", "32x", "sms", "gg", "pce", "a26", "a78", "lnx", "ngp", "ngc",
    "ws", "wsc", "pbp", "cso", "vpk", "xex", "rpx", "wux", "wua", "elf", "dsk", "adf", "d64", "tap", "tzx", "cdi", "gdi",
];

fn is_rom(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ROM_TYPES.contains(&e.to_lowercase().as_str()))
}

/// ES-DE's ROM folder, from its settings file, when ES-DE is here.
fn esde_rom_folder() -> Option<PathBuf> {
    let settings = [home().join("ES-DE/settings/es_settings.xml"), home().join(".emulationstation/es_settings.xml")]
        .into_iter()
        .find(|p| p.is_file())?;
    let text = fs::read_to_string(settings).ok()?;
    let line = text.lines().find(|l| l.contains("name=\"ROMDirectory\""))?;
    let value = line.split("value=\"").nth(1)?.split('"').next()?;
    let value = value.replace('~', &home().display().to_string());
    (!value.is_empty()).then(|| PathBuf::from(value)).filter(|p| p.is_dir())
}

fn system_roms(system: &str, dir: &Path, depth: usize, out: &mut Vec<Found>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            // A game kept as a folder (`.ps3`, multi-disc folders) or a subfolder of ROMs.
            if depth < 2 && !entry.file_name().to_string_lossy().starts_with('.') {
                system_roms(system, &path, depth + 1, out);
            }
            continue;
        }
        if !is_rom(&path) {
            continue;
        }
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let key = format!("rom:{system}/{}", title_key(&stem).trim_start_matches("title:"));
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(Found {
            game: ScannedGame {
                key,
                title: stem,
                platform: Some(system.to_string()),
                install: Install {
                    installed: true,
                    path: Some(path.display().to_string()),
                    size,
                    launcher: Some("emulator".into()),
                    ..Default::default()
                },
            },
            base: path.parent().map(Path::to_path_buf),
            prefix: None,
        });
    }
}

pub fn scan(settings: &Settings) -> Result<Vec<Found>, String> {
    let mut roots = settings.rom_folders.clone();
    if let Some(esde) = esde_rom_folder() {
        if !roots.contains(&esde) {
            roots.push(esde);
        }
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(read) = fs::read_dir(&root) else { continue };
        for entry in read.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                let system = entry.file_name().to_string_lossy().to_lowercase();
                system_roms(&system, &entry.path(), 0, &mut out);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roms_are_keyed_by_system_and_name() {
        let root = std::env::temp_dir().join(format!("dta-roms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("snes")).unwrap();
        fs::write(root.join("snes/Super Game (USA).sfc"), b"rom").unwrap();
        fs::write(root.join("snes/Super Game (USA).srm"), b"save").unwrap();
        let settings = Settings { rom_folders: vec![root.clone()], ..Default::default() };
        let found = scan(&settings).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].game.key, "rom:snes/super game usa");
        assert_eq!(found[0].game.platform.as_deref(), Some("snes"));
        let _ = fs::remove_dir_all(&root);
    }
}
