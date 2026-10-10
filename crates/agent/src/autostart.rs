//! Starting the agent when the person signs in (docs/DESIGN.md decision 9:
//! off until the person turns it on). Per user on every system, so nothing
//! needs an administrator:
//! - Linux: a systemd user unit when the session has a systemd user
//!   instance, else an XDG autostart entry;
//! - macOS: a LaunchAgent;
//! - Windows: the per-user Run key.
//!
//! What starts is the window-and-tray program (`droidtop-agent-app`) when it
//! sits beside this one, else `droidtop-agent run`.

use std::path::{Path, PathBuf};

/// The name every system knows the autostart entry by.
pub const LABEL: &str = "dev.droidtop.agent";

/// The program and arguments to start: the app beside this program, or this
/// program serving.
pub fn command() -> std::io::Result<(PathBuf, Vec<String>)> {
    let me = std::env::current_exe()?;
    let app = me.with_file_name(if cfg!(windows) { "droidtop-agent-app.exe" } else { "droidtop-agent-app" });
    if app.is_file() || me == app {
        // In the tray only: the person opens the window when they want it.
        Ok((app, vec!["--hidden".into()]))
    } else {
        Ok((me, vec!["run".into()]))
    }
}

#[cfg_attr(windows, allow(dead_code))]
/// Whether the program started is the window-and-tray one, which needs the
/// person's desktop session.
fn is_app(program: &Path) -> bool {
    program.file_stem().is_some_and(|s| s.to_string_lossy() == "droidtop-agent-app")
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// A shell word for systemd's `ExecStart` and a desktop entry's `Exec`.
fn quoted(word: &str) -> String {
    if word.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:".contains(c)) {
        word.to_string()
    } else {
        format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "$$"))
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn command_line(program: &Path, args: &[String]) -> String {
    std::iter::once(quoted(&program.display().to_string())).chain(args.iter().map(|a| quoted(a))).collect::<Vec<_>>().join(" ")
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The systemd user unit: tied to the graphical session for the app, to the
/// user's own instance for the plain service.
pub fn systemd_unit(program: &Path, args: &[String]) -> String {
    let (after, wanted) = if is_app(program) {
        ("After=graphical-session.target\nPartOf=graphical-session.target\n", "graphical-session.target")
    } else {
        ("After=network-online.target\n", "default.target")
    };
    format!(
        "[Unit]\nDescription=droidtop-agent: keeps this computer in step with droidtop\n{after}\n[Service]\nExecStart={}\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy={wanted}\n",
        command_line(program, args)
    )
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The XDG autostart entry, for a session without a systemd user instance.
pub fn desktop_entry(program: &Path, args: &[String]) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=droidtop-agent\nComment=Keeps this computer in step with droidtop\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        command_line(program, args)
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
/// The LaunchAgent: started at sign-in, in the person's graphical session
/// for the app.
pub fn launch_agent(program: &Path, args: &[String]) -> String {
    let words: String = std::iter::once(program.display().to_string())
        .chain(args.iter().cloned())
        .map(|w| format!("\t\t<string>{}</string>\n", xml(&w)))
        .collect();
    let session = if is_app(program) { "\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n" } else { "" };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{LABEL}</string>\n\t<key>ProgramArguments</key>\n\t<array>\n{words}\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n{session}</dict>\n</plist>\n"
    )
}

#[cfg_attr(not(windows), allow(dead_code))]
/// The Run key's value.
pub fn run_value(program: &Path, args: &[String]) -> String {
    std::iter::once(format!("\"{}\"", program.display())).chain(args.iter().map(|a| format!("\"{a}\""))).collect::<Vec<_>>().join(" ")
}

#[cfg(target_os = "linux")]
mod system {
    use super::*;

    fn home() -> PathBuf {
        dirs::home_dir().unwrap_or_default()
    }
    fn unit_path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("systemd/user/droidtop-agent.service")
    }
    fn desktop_path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("autostart/droidtop-agent.desktop")
    }
    /// A systemd user instance runs for this session.
    fn systemd_user() -> bool {
        std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| Path::new(&d).join("systemd").is_dir())
    }
    fn systemctl(args: &[&str]) {
        let _ = std::process::Command::new("systemctl").arg("--user").args(args).status();
    }

    pub fn enabled() -> bool {
        unit_path().is_file() || desktop_path().is_file()
    }

    pub fn set(on: bool) -> Result<String, String> {
        if !on {
            if unit_path().is_file() {
                systemctl(&["disable", "droidtop-agent.service"]);
                std::fs::remove_file(unit_path()).map_err(|e| e.to_string())?;
                systemctl(&["daemon-reload"]);
            }
            if desktop_path().is_file() {
                std::fs::remove_file(desktop_path()).map_err(|e| e.to_string())?;
            }
            return Ok("droidtop-agent no longer starts when you sign in.".into());
        }
        let (program, args) = command().map_err(|e| e.to_string())?;
        if systemd_user() {
            write(&unit_path(), &systemd_unit(&program, &args))?;
            systemctl(&["daemon-reload"]);
            systemctl(&["enable", "droidtop-agent.service"]);
            Ok(format!("droidtop-agent starts when you sign in (systemd user unit {}).", unit_path().display()))
        } else {
            write(&desktop_path(), &desktop_entry(&program, &args))?;
            Ok(format!("droidtop-agent starts when you sign in ({}).", desktop_path().display()))
        }
    }
}

#[cfg(target_os = "macos")]
mod system {
    use super::*;

    fn plist_path() -> PathBuf {
        dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
    }

    pub fn enabled() -> bool {
        plist_path().is_file()
    }

    pub fn set(on: bool) -> Result<String, String> {
        if !on {
            if plist_path().is_file() {
                std::fs::remove_file(plist_path()).map_err(|e| e.to_string())?;
            }
            return Ok("droidtop-agent no longer starts when you sign in.".into());
        }
        let (program, args) = command().map_err(|e| e.to_string())?;
        write(&plist_path(), &launch_agent(&program, &args))?;
        Ok(format!("droidtop-agent starts when you sign in (LaunchAgent {}).", plist_path().display()))
    }
}

#[cfg(windows)]
mod system {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "droidtop-agent";

    pub fn enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN, KEY_READ).is_ok_and(|k| k.get_value::<String, _>(VALUE).is_ok())
    }

    pub fn set(on: bool) -> Result<String, String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey_with_flags(RUN, KEY_READ | KEY_WRITE).map_err(|e| e.to_string())?;
        if !on {
            let _ = key.delete_value(VALUE);
            return Ok("droidtop-agent no longer starts when you sign in.".into());
        }
        let (program, args) = command().map_err(|e| e.to_string())?;
        key.set_value(VALUE, &run_value(&program, &args)).map_err(|e| e.to_string())?;
        Ok("droidtop-agent starts when you sign in.".into())
    }
}

#[cfg(not(windows))]
fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether the agent starts when the person signs in.
pub fn enabled() -> bool {
    system::enabled()
}

/// Turns starting at sign-in on or off; says what it did.
pub fn set(on: bool) -> Result<String, String> {
    system::set(on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_system_gets_its_own_entry() {
        let app = Path::new("/opt/droidtop agent/droidtop-agent-app");
        let unit = systemd_unit(app, &[]);
        assert!(unit.contains("ExecStart=\"/opt/droidtop agent/droidtop-agent-app\""));
        assert!(unit.contains("WantedBy=graphical-session.target"));
        let cli = systemd_unit(Path::new("/usr/bin/droidtop-agent"), &["run".into()]);
        assert!(cli.contains("ExecStart=/usr/bin/droidtop-agent run") && cli.contains("WantedBy=default.target"));
        assert!(desktop_entry(app, &[]).contains("Exec=\"/opt/droidtop agent/droidtop-agent-app\""));
        let plist = launch_agent(Path::new("/Applications/droidtop-agent.app/Contents/MacOS/droidtop-agent-app"), &[]);
        assert!(plist.contains("<string>dev.droidtop.agent</string>") && plist.contains("Aqua"));
        assert!(plist::Value::from_reader_xml(plist.as_bytes()).is_ok());
        assert_eq!(run_value(Path::new(r"C:\Tools\droidtop-agent.exe"), &["run".into()]), r#""C:\Tools\droidtop-agent.exe" "run""#);
    }
}
