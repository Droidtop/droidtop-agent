//! Starting a game on this computer for a paired handheld (docs/DESIGN.md
//! section 17). The handheld names a game by its key; this program builds
//! what it runs from that key and nothing else, so no text from the handheld
//! is ever executed: `steam:<digits>` becomes `steam://rungameid/<digits>`,
//! handed to the system's URL handler, and any other key is refused.

use droidtop_agent_core::proto::{GameRef, Launch};

/// What this decision needs from the system, so it can be tested without
/// starting Steam.
pub trait System {
    /// Whether the Steam client is running now.
    fn steam_running(&self) -> bool;
    /// Hands [`url`] to the system's URL handler.
    fn open(&self, url: &str) -> Result<(), String>;
}

/// The URL that starts a Steam game, from a `steam:<appid>` key: the app id
/// must be one to ten ASCII digits. Anything else gives nothing.
pub fn steam_url(key: &str) -> Option<String> {
    let id = key.strip_prefix("steam:")?;
    (!id.is_empty() && id.len() <= 10 && id.bytes().all(|b| b.is_ascii_digit())).then(|| format!("steam://rungameid/{id}"))
}

/// Starts [`game`] if [`allowed`] (the person let this handheld start games
/// here) and [`installed`] (the scan found it on this computer). The order is
/// the order of what a handheld may learn: one that is not allowed learns
/// nothing about this computer's games.
pub fn launch(allowed: bool, installed: impl FnOnce() -> bool, game: &GameRef, os: &dyn System) -> Launch {
    if !allowed {
        return Launch::NotAllowed;
    }
    let Some(url) = steam_url(&game.key) else { return Launch::UnknownGame };
    if !installed() {
        return Launch::UnknownGame;
    }
    // Steam Link streams from a running Steam; opening the URL with Steam
    // closed would only start the client, and a sign-in could wait behind it.
    if !os.steam_running() {
        return Launch::SteamNotRunning;
    }
    match os.open(&url) {
        Ok(()) => Launch::Started,
        Err(message) => Launch::Failed { message },
    }
}

/// This computer's own Steam and URL handler.
pub struct Local;

impl System for Local {
    fn steam_running(&self) -> bool {
        steam_running()
    }

    fn open(&self, url: &str) -> Result<(), String> {
        let mut command = opener(url);
        let status = command.status().map_err(|e| format!("could not run the system's URL handler ({e})"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("the system's URL handler stopped with {status}"))
        }
    }
}

#[cfg(windows)]
fn opener(url: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = std::process::Command::new("rundll32.exe");
    c.args(["url.dll,FileProtocolHandler", url]).creation_flags(0x0800_0000);
    c
}

#[cfg(target_os = "macos")]
fn opener(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("open");
    c.arg(url);
    c
}

#[cfg(not(any(windows, target_os = "macos")))]
fn opener(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("xdg-open");
    c.arg(url);
    c
}

#[cfg(windows)]
fn steam_running() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq steam.exe", "/NH", "/FO", "CSV"])
        .creation_flags(0x0800_0000)
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("\"steam.exe\""))
}

#[cfg(target_os = "macos")]
fn steam_running() -> bool {
    std::process::Command::new("pgrep").args(["-x", "steam_osx"]).status().is_ok_and(|s| s.success())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn steam_running() -> bool {
    // The client's main process is named `steam`, natively and in Flatpak
    // (whose processes show in the host's /proc).
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|entry| {
        entry.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())
            && std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|c| c.trim() == "steam")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        running: bool,
        fail: Option<&'static str>,
        opened: RefCell<Vec<String>>,
    }

    impl System for Fake {
        fn steam_running(&self) -> bool {
            self.running
        }
        fn open(&self, url: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(url.to_string());
            self.fail.map_or(Ok(()), |m| Err(m.to_string()))
        }
    }

    fn fake(running: bool, fail: Option<&'static str>) -> Fake {
        Fake { running, fail, opened: RefCell::new(Vec::new()) }
    }

    fn game(key: &str) -> GameRef {
        GameRef { key: key.into(), title: "Some Game".into() }
    }

    #[test]
    fn only_a_numeric_steam_key_makes_a_url() {
        assert_eq!(steam_url("steam:440").as_deref(), Some("steam://rungameid/440"));
        for bad in [
            "steam:",
            "steam:abc",
            "steam:440 ",
            "steam:44/0",
            "steam:440&x=1",
            "steam:../../x",
            "steam:-1",
            "steam:12345678901",
            "steam://rungameid/440",
            "STEAM:440",
            "gog:440",
            "title:doom",
            "440",
            "",
            "steam:١٢٣",
        ] {
            assert_eq!(steam_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_handheld_that_is_not_allowed_starts_nothing_and_learns_nothing() {
        let os = fake(true, None);
        for key in ["steam:440", "steam:abc", "title:x"] {
            assert_eq!(launch(false, || panic!("the scan is not asked"), &game(key), &os), Launch::NotAllowed);
        }
        assert!(os.opened.borrow().is_empty());
    }

    #[test]
    fn a_key_it_cannot_map_or_a_game_not_installed_is_unknown() {
        let os = fake(true, None);
        assert_eq!(launch(true, || true, &game("title:doom"), &os), Launch::UnknownGame);
        assert_eq!(launch(true, || true, &game("steam:440; calc"), &os), Launch::UnknownGame);
        assert_eq!(launch(true, || false, &game("steam:440"), &os), Launch::UnknownGame);
        assert!(os.opened.borrow().is_empty());
    }

    #[test]
    fn nothing_starts_while_steam_is_not_running() {
        let os = fake(false, None);
        assert_eq!(launch(true, || true, &game("steam:440"), &os), Launch::SteamNotRunning);
        assert!(os.opened.borrow().is_empty());
    }

    #[test]
    fn an_allowed_installed_game_is_handed_to_the_url_handler_once() {
        let os = fake(true, None);
        assert_eq!(launch(true, || true, &game("steam:440"), &os), Launch::Started);
        assert_eq!(*os.opened.borrow(), ["steam://rungameid/440"]);
    }

    #[test]
    fn a_url_handler_that_fails_is_reported() {
        let os = fake(true, Some("no handler"));
        assert_eq!(launch(true, || true, &game("steam:440"), &os), Launch::Failed { message: "no handler".into() });
    }
}
