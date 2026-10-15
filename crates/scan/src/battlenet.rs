//! Battle.net (Windows): Blizzard's games list themselves in the uninstall
//! registry with Blizzard as the publisher. The Battle.net app itself is
//! left out. Elsewhere Battle.net games run through Lutris, which the Lutris
//! scan finds.

use super::Found;
use crate::state::Settings;

#[cfg(windows)]
pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    use std::path::PathBuf;
    use winreg::enums::HKEY_LOCAL_MACHINE;
    let mut out = Vec::new();
    for path in [r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"]
    {
        let Ok(list) = winreg::RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) else { continue };
        for id in list.enum_keys().flatten() {
            let Ok(k) = list.open_subkey(&id) else { continue };
            let publisher: String = k.get_value("Publisher").unwrap_or_default();
            let title: String = k.get_value("DisplayName").unwrap_or_default();
            if !publisher.starts_with("Blizzard") || title.is_empty() || title == "Battle.net" {
                continue;
            }
            let base = k.get_value::<String, _>("InstallLocation").ok().map(PathBuf::from).filter(|p| p.is_dir());
            let version = k.get_value::<String, _>("DisplayVersion").ok();
            out.push(super::pc_game(format!("battlenet:{id}"), title, base, "battlenet", 0, version));
        }
    }
    Ok(out)
}

#[cfg(not(windows))]
pub fn scan(_settings: &Settings) -> Result<Vec<Found>, String> {
    Ok(Vec::new())
}
