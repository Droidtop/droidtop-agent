//! Wine and Proton prefixes on Linux and macOS: where a Windows game that
//! runs here keeps its Windows saves (docs/DESIGN.md section 6). Steam's own
//! Proton prefixes are found by the Steam scan; this covers the launchers that
//! keep a prefix per game elsewhere: Heroic (GOG, Epic, Amazon), Lutris and
//! Bottles.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{env_dir, home, Prefix};

/// The prefix at [`path`]: a Wine prefix (`drive_c` in it) or a Proton one
/// (`pfx/drive_c`). The Windows user is Proton's `steamuser` when that is
/// there, else the one user folder that is not `Public`.
pub fn prefix_at(path: &Path) -> Option<Prefix> {
    let drive_c = [path.join("drive_c"), path.join("pfx/drive_c")].into_iter().find(|p| p.is_dir())?;
    let users: Vec<String> = fs::read_dir(drive_c.join("users"))
        .map(|r| r.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let user = if users.iter().any(|u| u == "steamuser") {
        "steamuser".to_string()
    } else {
        let mut own: Vec<&String> = users.iter().filter(|u| !u.eq_ignore_ascii_case("public")).collect();
        own.sort();
        match own.first() {
            Some(u) => u.to_string(),
            None => std::env::var("USER").unwrap_or_else(|_| "steamuser".into()),
        }
    };
    Some(Prefix { drive_c, user })
}

/// Heroic's settings folders on each system.
pub fn heroic_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut out = vec![
        h.join(".config/heroic"),
        h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"),
        h.join("Library/Application Support/heroic"),
    ];
    if let Some(appdata) = env_dir("APPDATA") {
        out.push(appdata.join("heroic"));
    }
    out
}

/// The prefix Heroic runs a game in: `GamesConfig/<app name>.json` says
/// `{"<app name>": {"winePrefix": ...}}`.
pub fn heroic(dirs: &[PathBuf], app_name: &str) -> Option<Prefix> {
    for dir in dirs {
        let Ok(text) = fs::read_to_string(dir.join("GamesConfig").join(format!("{app_name}.json"))) else { continue };
        let Ok(config) = serde_json::from_str::<Value>(&text) else { continue };
        if let Some(prefix) = config[app_name]["winePrefix"].as_str().and_then(|p| prefix_at(Path::new(p))) {
            return Some(prefix);
        }
    }
    None
}

/// Lutris's per-game settings folders (older and newer Lutris, and its Flatpak).
pub fn lutris_game_dirs() -> Vec<PathBuf> {
    let h = home();
    vec![
        h.join(".config/lutris/games"),
        h.join(".local/share/lutris/games"),
        h.join(".var/app/net.lutris.Lutris/config/lutris/games"),
        h.join(".var/app/net.lutris.Lutris/data/lutris/games"),
    ]
}

/// The prefix of a Lutris Wine game: `game: prefix:` in `<configpath>.yml`.
pub fn lutris(dirs: &[PathBuf], configpath: &str) -> Option<Prefix> {
    for dir in dirs {
        let Ok(text) = fs::read_to_string(dir.join(format!("{configpath}.yml"))) else { continue };
        let Ok(config) = serde_yaml::from_str::<serde_yaml::Value>(&text) else { continue };
        if let Some(prefix) = config["game"]["prefix"].as_str().and_then(|p| prefix_at(Path::new(&expand_home(p)))) {
            return Some(prefix);
        }
    }
    None
}

/// `~/x` as the person's home folder.
fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest).display().to_string(),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dtagent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn wine_and_proton_prefixes_and_their_users() {
        let dir = tmp("prefixes");
        fs::create_dir_all(dir.join("wine/drive_c/users/Public")).unwrap();
        fs::create_dir_all(dir.join("wine/drive_c/users/mech")).unwrap();
        fs::create_dir_all(dir.join("proton/pfx/drive_c/users/steamuser")).unwrap();
        let wine = prefix_at(&dir.join("wine")).unwrap();
        assert_eq!((wine.drive_c, wine.user), (dir.join("wine/drive_c"), "mech".to_string()));
        assert_eq!(prefix_at(&dir.join("proton")).unwrap().user, "steamuser");
        assert!(prefix_at(&dir.join("none")).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn heroic_and_lutris_name_a_games_prefix() {
        let dir = tmp("launchers");
        let prefix = dir.join("Prefixes/Game");
        fs::create_dir_all(prefix.join("drive_c/users/me")).unwrap();
        fs::create_dir_all(dir.join("heroic/GamesConfig")).unwrap();
        let config = serde_json::json!({ "1207658924": { "winePrefix": prefix.display().to_string() } });
        fs::write(dir.join("heroic/GamesConfig/1207658924.json"), config.to_string()).unwrap();
        assert_eq!(heroic(&[dir.join("heroic")], "1207658924").unwrap().drive_c, prefix.join("drive_c"));
        assert!(heroic(&[dir.join("heroic")], "other").is_none());
        fs::create_dir_all(dir.join("lutris")).unwrap();
        fs::write(dir.join("lutris/game-123.yml"), format!("game:\n  exe: x.exe\n  prefix: {}\nwine:\n  version: ge\n", prefix.display()))
            .unwrap();
        assert_eq!(lutris(&[dir.join("lutris")], "game-123").unwrap().user, "me");
        fs::remove_dir_all(&dir).unwrap();
    }
}
