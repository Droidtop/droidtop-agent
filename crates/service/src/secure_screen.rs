//! Service mode: windowcast's secure-screen helper (windowcast#467,
//! docs/DESIGN.md section 19). A small fixed-purpose program that runs as
//! SYSTEM so this computer's Windows security screens (UAC prompts, the lock
//! and sign-in screen) can be shown and answered from a paired device. The
//! owner, on #467: "We won't just run code as SYSTEM without user approval,
//! we'll ask, and let them enable/disable in settings at their own leisure."
//!
//! So it is off until the person turns it on, after [`CONSENT`] says what
//! that means, and every change goes through Windows' own administrator
//! approval (UAC): this program asks Windows to run `program args` again
//! elevated ([`request`]), and that elevated copy calls [`perform`], which is
//! windowcast's client API: install the helper into Program Files, then
//! enable (bound to the person's own account), disable or uninstall.

use std::path::{Path, PathBuf};

pub use windowcast_secure_helper::SecureScreenStatus;
use windowcast_secure_helper::{permission, Client};

/// What the switch says before it is turned on.
pub const CONSENT: &str = "Service mode lets a paired device see and answer this computer's Windows security \
screens: User Account Control prompts and the lock and sign-in screen, which ordinary programs cannot see. \
To do that, windowcast's small secure-screen helper is installed in Program Files and runs as SYSTEM. It does \
only three things: show the security screen, type and click on it, and say whether one is open; and it answers \
only installed programs running under your Windows account. Windows asks for administrator approval to turn \
it on, and again to turn it off. You can turn it off or remove it here at any time.";

/// The helper's file name beside a program that ships it.
pub const HELPER: &str = "windowcast-secure-helper.exe";
/// Its name in droidtop-agent's release files.
pub const HELPER_RELEASED: &str = "windowcast-secure-helper-windows-x86_64.exe";

/// What a change does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Installs the helper (when it is not there) and enables it.
    Enable,
    /// Turns it off: its permission and its SYSTEM task are removed.
    Disable,
    /// Turns it off and removes the helper.
    Uninstall,
}

impl Action {
    pub fn parse(word: &str) -> Option<Action> {
        match word {
            "enable" => Some(Action::Enable),
            "disable" => Some(Action::Disable),
            "uninstall" => Some(Action::Uninstall),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Action::Enable => "enable",
            Action::Disable => "disable",
            Action::Uninstall => "uninstall",
        }
    }
}

/// `not_available` (not Windows), `absent` (not installed), `disabled` or `enabled`.
pub fn state() -> &'static str {
    match Client::default().permission_status() {
        SecureScreenStatus::NotAvailable => "not_available",
        SecureScreenStatus::Absent => "absent",
        SecureScreenStatus::Disabled => "disabled",
        SecureScreenStatus::Enabled => "enabled",
    }
}

/// The helper this program ships: in `dir`, under its own name or its
/// release name.
pub fn helper_beside(dir: &Path) -> Option<PathBuf> {
    [HELPER, HELPER_RELEASED].iter().map(|name| dir.join(name)).find(|p| p.is_file())
}

/// The exit codes of the elevated copy, so the one that asked can say what happened.
pub mod exit {
    pub const DONE: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const NO_HELPER: i32 = 2;
    pub const BAD_REQUEST: i32 = 3;
}

/// From the person's own, unelevated program: asks Windows to run
/// `program args.. <action> --confirmed [--grant-sid <sid>]` with
/// administrator approval, waits for it, and says what happened. Enabling
/// binds the permission to this account's SID, read here before elevation
/// (an administrator's elevated token could belong to another account).
/// The caller shows [`CONSENT`] before enabling.
pub fn request(action: Action, program: &Path, args: &[&str]) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("Service mode is only on Windows.".into());
    }
    let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    all.push(action.word().into());
    all.push("--confirmed".into());
    if action == Action::Enable {
        let sid = permission::current_user_sid().map_err(|e| format!("Your Windows account could not be read: {e}"))?;
        if !permission::validate_grant_sid(&sid) {
            return Err("Windows gave an account id that is not valid.".into());
        }
        all.push("--grant-sid".into());
        all.push(sid);
    }
    let code = run_elevated(program, &all)?;
    match code {
        exit::DONE => Ok(match action {
            Action::Enable => "Service mode is on.".into(),
            Action::Disable => "Service mode is off; the helper stays installed.".into(),
            Action::Uninstall => "Service mode is off and the helper is removed.".into(),
        }),
        exit::NO_HELPER => Err("This copy has no secure-screen helper to install beside it.".into()),
        _ => Err(format!("The change did not complete (code {code}); Windows' Event Viewer has the helper's log.")),
    }
}

/// In the elevated copy: does `action` through windowcast's client API.
/// `helper` is the helper to install when it is not installed yet.
pub fn perform(action: Action, grant_sid: Option<&str>, helper: Option<&Path>) -> Result<(), (i32, String)> {
    let client = Client::default();
    let failed = |e: windowcast_secure_helper::SecureScreenError| (exit::FAILED, e.to_string());
    match action {
        Action::Enable => {
            let sid = grant_sid
                .filter(|s| permission::validate_grant_sid(s))
                .ok_or((exit::BAD_REQUEST, "no valid account id to grant".into()))?;
            if !client.helper_binary_exists() {
                let helper = helper.ok_or((exit::NO_HELPER, "no helper beside this program".into()))?;
                client.install_binary(helper).map_err(failed)?;
            }
            client.enable(sid).map_err(failed)
        }
        Action::Disable => client.disable().map_err(failed),
        Action::Uninstall => client.uninstall().map_err(failed),
    }
}

#[cfg(windows)]
fn run_elevated(program: &Path, args: &[String]) -> Result<i32, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let file = wide(program.as_os_str());
    let params = wide(std::ffi::OsStr::new(&args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")));
    let verb = wide(std::ffi::OsStr::new("runas"));
    // The elevated copy starts in Windows' own folder, never one the person can write.
    let dir = wide(std::ffi::OsStr::new(
        &std::env::var("SystemRoot").map(|r| format!("{r}\\System32")).unwrap_or_else(|_| r"C:\Windows\System32".into()),
    ));
    // SAFETY: every pointer is to a NUL-terminated buffer that outlives the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.lpDirectory = dir.as_ptr();
    info.nShow = SW_HIDE;
    // Microsoft: ShellExecuteEx with the "runas" verb asks UAC to start the program elevated.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(if unsafe { GetLastError() } == ERROR_CANCELLED {
            "Administrator approval was not given; nothing changed.".into()
        } else {
            format!("Windows could not ask for administrator approval: {}", std::io::Error::last_os_error())
        });
    }
    if info.hProcess.is_null() {
        return Err("Windows did not say how the change went.".into());
    }
    let mut code = 1u32;
    // SAFETY: hProcess is the process ShellExecuteEx opened for us; we close it once.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code as i32)
}

#[cfg(not(windows))]
fn run_elevated(_program: &Path, _args: &[String]) -> Result<i32, String> {
    Err("Service mode is only on Windows.".into())
}

/// One command-line argument as Windows' own parser reads it back
/// (CommandLineToArgvW): quoted when it has a space, tab or quote, with
/// backslashes before a quote doubled.
#[cfg_attr(not(windows), allow(dead_code))]
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_as_windows_reads_them() {
        assert_eq!(quote("enable"), "enable");
        assert_eq!(quote(r"C:\Program Files\Pythia"), r#""C:\Program Files\Pythia""#);
        assert_eq!(quote(r#"a "b""#), r#""a \"b\"""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn actions_read_back_their_words() {
        for a in [Action::Enable, Action::Disable, Action::Uninstall] {
            assert_eq!(Action::parse(a.word()), Some(a));
        }
        assert_eq!(Action::parse("install"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_it_says_so_and_changes_nothing() {
        assert_eq!(state(), "not_available");
        assert!(request(Action::Enable, Path::new("/bin/true"), &[]).unwrap_err().contains("only on Windows"));
    }

    #[test]
    fn the_helper_is_found_under_either_name() {
        let dir = std::env::temp_dir().join(format!("dta-secure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(helper_beside(&dir), None);
        std::fs::write(dir.join(HELPER_RELEASED), b"").unwrap();
        assert_eq!(helper_beside(&dir), Some(dir.join(HELPER_RELEASED)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
