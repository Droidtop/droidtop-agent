//! The applications installed on this computer, games or not (docs/DESIGN.md
//! section 11, "Installed applications"): the owner's "we need to track third
//! party apps and stuff too". Read from what each system keeps about its
//! installs; nothing is run and nothing goes to the network.
//!
//! - Windows: the uninstall registry (both views of HKLM, and HKCU) and the
//!   Store (MSIX/AppX) packages registered for this user.
//! - Linux: XDG desktop entries, including Flatpak's and Snap's exports.
//! - macOS: `.app` bundles in the Applications folders, from their
//!   `Info.plist`.
//!
//! Each becomes a library entry of the platform `app`, keyed
//! `app:<source>:<id>`, so it travels in the same change log as the games.
//! The parsers are plain functions over text and values, so CI tests all of
//! them on every system; only the readers depend on the system.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use droidtop_agent_core::library::{Install, ScannedGame};
use serde::Serialize;

/// The platform installed applications carry in the library.
pub const PLATFORM: &str = "app";

/// One installed application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledApp {
    /// Where the system records it: `win`, `msix`, `desktop`, `flatpak`,
    /// `snap` or `mac`.
    pub source: String,
    /// Its id within that source: the uninstall key, the package name, the
    /// desktop file id, the Flatpak or Snap name, the bundle id.
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Its folder or bundle, when the system says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl InstalledApp {
    pub fn key(&self) -> String {
        format!("app:{}:{}", self.source, self.id)
    }

    /// The source as the handheld shows it beside the name.
    pub fn source_label(&self) -> &'static str {
        match self.source.as_str() {
            "win" => "Windows",
            "msix" => "Microsoft Store",
            "flatpak" => "Flatpak",
            "snap" => "Snap",
            "mac" => "macOS",
            _ => "Linux",
        }
    }

    pub fn to_library(&self) -> ScannedGame {
        ScannedGame {
            key: self.key(),
            title: self.name.clone(),
            platform: Some(PLATFORM.into()),
            install: Install {
                installed: true,
                path: self.path.as_ref().map(|p| p.display().to_string()),
                version: self.version.clone(),
                launcher: Some(self.source_label().to_string()),
                ..Default::default()
            },
        }
    }
}

/// Whether [`path`] is inside one of the game folders the scan found: a
/// store's own uninstall entry or shortcut for a game is the game, not an app.
fn inside_a_game(path: &Path, game_folders: &[PathBuf]) -> bool {
    let lower = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase();
    let path = lower(path);
    !path.is_empty()
        && game_folders.iter().map(|g| lower(g)).filter(|g| !g.is_empty()).any(|g| path == g || path.starts_with(&format!("{g}/")))
}

/// The applications installed here, leaving out those inside [`game_folders`].
pub fn scan(game_folders: &[PathBuf]) -> Vec<InstalledApp> {
    let mut by_key: BTreeMap<String, InstalledApp> = BTreeMap::new();
    for app in read() {
        if app.path.as_deref().is_some_and(|p| inside_a_game(p, game_folders)) {
            continue;
        }
        by_key.entry(app.key()).or_insert(app);
    }
    by_key.into_values().collect()
}

#[cfg(windows)]
fn read() -> Vec<InstalledApp> {
    let mut out = windows::uninstall_entries();
    out.extend(windows::store_packages());
    out
}

#[cfg(target_os = "macos")]
fn read() -> Vec<InstalledApp> {
    mac::bundles(&mac::folders())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read() -> Vec<InstalledApp> {
    linux::entries(&linux::folders())
}

/// The uninstall registry, as values read from one entry.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod uninstall {
    use super::InstalledApp;
    use std::path::PathBuf;

    /// The values of one uninstall entry that matter here.
    #[derive(Debug, Default, Clone)]
    pub struct Entry {
        /// The entry's key name: a product GUID or a name.
        pub key: String,
        pub display_name: Option<String>,
        pub display_version: Option<String>,
        pub publisher: Option<String>,
        pub install_location: Option<String>,
        pub system_component: bool,
        pub parent_key_name: Option<String>,
        pub release_type: Option<String>,
    }

    /// Runtimes and redistributables other programs install for themselves:
    /// they are parts of something else, not applications a person uses.
    const PARTS: &[&str] = &[
        "redistributable",
        "microsoft .net",
        "microsoft windows desktop runtime",
        "microsoft asp.net",
        "microsoft visual c++",
        "directx",
        "vulkan runtime",
        "windows software development kit",
        "windows sdk",
        "microsoft update health tools",
        "update for windows",
    ];

    /// What an entry is as an application, or `None` when it is a part of
    /// another program, an update, a system component, or a store's entry for
    /// one of its games (Steam writes "Steam App <id>").
    pub fn app(entry: &Entry) -> Option<InstalledApp> {
        let name = entry.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty())?;
        if entry.system_component || entry.parent_key_name.as_deref().is_some_and(|p| !p.is_empty()) {
            return None;
        }
        if entry.release_type.as_deref().is_some_and(|r| {
            let r = r.to_lowercase();
            r.contains("update") || r.contains("hotfix")
        }) {
            return None;
        }
        let lower = name.to_lowercase();
        if lower.starts_with("steam app ") || PARTS.iter().any(|p| lower.contains(p)) {
            return None;
        }
        let id = entry.key.trim().trim_start_matches('{').trim_end_matches('}').to_string();
        if id.is_empty() {
            return None;
        }
        let path = entry.install_location.as_deref().map(|p| p.trim().trim_matches('"')).filter(|p| !p.is_empty()).map(PathBuf::from);
        Some(InstalledApp {
            source: "win".into(),
            id,
            name: name.to_string(),
            version: entry.display_version.clone().filter(|v| !v.trim().is_empty()),
            publisher: entry.publisher.clone().filter(|p| !p.trim().is_empty()),
            path,
        })
    }
}

/// Store (MSIX/AppX) packages, from their `AppxManifest.xml`.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod msix {
    use super::InstalledApp;
    use std::path::PathBuf;

    /// What a package manifest says, or `None` for a framework, a resource
    /// package, or a package with no application the Start menu lists.
    pub fn app(manifest: &str, root: PathBuf) -> Option<InstalledApp> {
        let doc = roxmltree::Document::parse(manifest).ok()?;
        let element = |name: &str| doc.descendants().find(|n| n.is_element() && n.tag_name().name() == name);
        let text = |name: &str| element(name).and_then(|n| n.text()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let yes = |name: &str| text(name).is_some_and(|t| t.eq_ignore_ascii_case("true"));
        if yes("Framework") || yes("ResourcePackage") {
            return None;
        }
        let identity = element("Identity")?;
        let package = identity.attribute("Name")?.to_string();
        let listed = doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "Application").any(|application| {
            !application
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "VisualElements")
                .any(|v| v.attribute("AppListEntry").is_some_and(|e| e.eq_ignore_ascii_case("none")))
        });
        if !listed {
            return None;
        }
        // An `ms-resource:` name has to be looked up in the package's
        // resources; the package name stands in for it.
        let name = text("DisplayName").filter(|n| !n.starts_with("ms-resource:")).unwrap_or_else(|| package.clone());
        Some(InstalledApp {
            source: "msix".into(),
            id: package,
            name,
            version: identity.attribute("Version").map(str::to_string),
            publisher: text("PublisherDisplayName").filter(|p| !p.starts_with("ms-resource:")),
            path: Some(root),
        })
    }
}

/// XDG desktop entries.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub mod desktop {
    use super::InstalledApp;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Shortcuts launchers make for their games: the game is already in the
    /// library under its store key.
    const GAME_SHORTCUTS: &[&str] =
        &["steam://rungameid/", "heroic://launch", "lutris:rungame", "itch://", "minigalaxy --launch", "bottles-cli run"];

    /// The `[Desktop Entry]` group's keys (unlocalised).
    pub fn parse(text: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let mut in_entry = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                if !k.contains('[') {
                    out.entry(k.to_string()).or_insert_with(|| v.trim().to_string());
                }
            }
        }
        out
    }

    /// The application a desktop file describes. [`id`] is its desktop file
    /// id (`org.gnome.Calculator`, without `.desktop`); [`snap_dir`] says it
    /// came from Snap's own folder.
    pub fn app(id: &str, text: &str, snap_dir: bool) -> Option<InstalledApp> {
        let e = parse(text);
        let yes = |k: &str| e.get(k).is_some_and(|v| v.eq_ignore_ascii_case("true"));
        if e.get("Type").map(String::as_str) != Some("Application") || yes("NoDisplay") || yes("Hidden") {
            return None;
        }
        let name = e.get("Name").filter(|n| !n.is_empty())?.clone();
        let exec = e.get("Exec").cloned().unwrap_or_default();
        if GAME_SHORTCUTS.iter().any(|s| exec.contains(s)) {
            return None;
        }
        let (source, id) = if let Some(flatpak) = e.get("X-Flatpak").filter(|f| !f.is_empty()) {
            ("flatpak", flatpak.clone())
        } else if let Some(snap) = e.get("X-SnapInstanceName").filter(|s| !s.is_empty()) {
            ("snap", snap.clone())
        } else if snap_dir {
            // Snap names its exports `<snap>_<app>.desktop`.
            ("snap", id.split('_').next().unwrap_or(id).to_string())
        } else {
            ("desktop", id.to_string())
        };
        Some(InstalledApp {
            source: source.into(),
            id,
            name,
            version: e.get("X-AppVersion").cloned(),
            publisher: None,
            path: e.get("Path").filter(|p| !p.is_empty()).map(PathBuf::from),
        })
    }
}

/// macOS application bundles.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod bundle {
    use super::InstalledApp;
    use std::path::Path;

    /// The application a bundle's `Info.plist` describes (XML or binary).
    pub fn app(bundle: &Path, info_plist: &[u8]) -> Option<InstalledApp> {
        let value = plist::Value::from_reader(std::io::Cursor::new(info_plist)).ok()?;
        let dict = value.as_dictionary()?;
        let get = |k: &str| dict.get(k).and_then(|v| v.as_string()).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        // A background agent or a plug-in is not an application of its own.
        if get("CFBundlePackageType").is_some_and(|t| t != "APPL") {
            return None;
        }
        let stem = bundle.file_stem()?.to_string_lossy().into_owned();
        let name = get("CFBundleDisplayName").or_else(|| get("CFBundleName")).unwrap_or_else(|| stem.clone());
        Some(InstalledApp {
            source: "mac".into(),
            id: get("CFBundleIdentifier").unwrap_or(stem),
            name,
            version: get("CFBundleShortVersionString").or_else(|| get("CFBundleVersion")),
            publisher: None,
            path: Some(bundle.to_path_buf()),
        })
    }
}

#[cfg(windows)]
mod windows {
    use super::{msix, uninstall, InstalledApp};
    use std::path::PathBuf;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    const PACKAGES: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

    pub fn uninstall_entries() -> Vec<InstalledApp> {
        let views = [
            (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_64KEY),
            (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_32KEY),
            (HKEY_CURRENT_USER, KEY_READ),
        ];
        let mut out = Vec::new();
        for (hive, flags) in views {
            let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, flags) else { continue };
            for name in root.enum_keys().flatten() {
                let Ok(key) = root.open_subkey_with_flags(&name, flags) else { continue };
                let s = |v: &str| key.get_value::<String, _>(v).ok();
                let entry = uninstall::Entry {
                    key: name.clone(),
                    display_name: s("DisplayName"),
                    display_version: s("DisplayVersion"),
                    publisher: s("Publisher"),
                    install_location: s("InstallLocation"),
                    system_component: key.get_value::<u32, _>("SystemComponent").is_ok_and(|v| v == 1),
                    parent_key_name: s("ParentKeyName"),
                    release_type: s("ReleaseType"),
                };
                out.extend(uninstall::app(&entry));
            }
        }
        out
    }

    pub fn store_packages() -> Vec<InstalledApp> {
        let Ok(root) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(PACKAGES) else { return Vec::new() };
        let mut out = Vec::new();
        for name in root.enum_keys().flatten() {
            let Ok(key) = root.open_subkey(&name) else { continue };
            let Ok(folder) = key.get_value::<String, _>("PackageRootFolder") else { continue };
            let folder = PathBuf::from(folder);
            // Windows' own system apps are part of Windows.
            if folder.to_string_lossy().to_lowercase().contains(r"\windows\systemapps") {
                continue;
            }
            let Ok(manifest) = std::fs::read_to_string(folder.join("AppxManifest.xml")) else { continue };
            out.extend(msix::app(&manifest, folder));
        }
        out
    }
}

#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub mod linux {
    use super::{desktop, InstalledApp};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// The `applications` folders to read, most important first: the
    /// person's own, the system's (`$XDG_DATA_DIRS`), then Flatpak's and
    /// Snap's exports in case the session does not list them. The flag marks
    /// Snap's folder.
    pub fn folders() -> Vec<(PathBuf, bool)> {
        let home = crate::scan::home();
        let data_home = crate::scan::env_dir("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let data_dirs =
            std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let mut roots = vec![data_home.clone()];
        roots.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
        roots.push(data_home.join("flatpak/exports/share"));
        roots.push(PathBuf::from("/var/lib/flatpak/exports/share"));
        let mut out: Vec<(PathBuf, bool)> = roots.into_iter().map(|r| (r.join("applications"), false)).collect();
        out.push((PathBuf::from("/var/lib/snapd/desktop/applications"), true));
        let mut seen = BTreeSet::new();
        out.retain(|(p, _)| seen.insert(p.clone()));
        out
    }

    /// The desktop entries in [`folders`]. The first folder to hold a desktop
    /// file id wins, as the XDG spec says.
    pub fn entries(folders: &[(PathBuf, bool)]) -> Vec<InstalledApp> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for (folder, snap) in folders {
            let mut files = Vec::new();
            walk(folder, folder, &mut files, 0);
            files.sort();
            for (id, path) in files {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                out.extend(desktop::app(&id, &text, *snap));
            }
        }
        out
    }

    /// Desktop files under [`dir`], with their ids (`kde/foo.desktop` is
    /// `kde-foo`).
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() && depth < 3 {
                walk(root, &path, out, depth + 1);
            } else if path.extension().is_some_and(|e| e == "desktop") {
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    out.push((rel.trim_end_matches(".desktop").replace('/', "-"), path));
                }
            }
        }
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod mac {
    use super::{bundle, InstalledApp};
    use std::path::{Path, PathBuf};

    pub fn folders() -> Vec<PathBuf> {
        let home = crate::scan::home();
        vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Utilities"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
            home.join("Applications"),
        ]
    }

    /// The `.app` bundles directly in [`folders`], and one folder down
    /// (vendors' own folders, like `/Applications/Blizzard`).
    pub fn bundles(folders: &[PathBuf]) -> Vec<InstalledApp> {
        let apps_in = |dir: &Path| -> Vec<PathBuf> {
            let mut out: Vec<PathBuf> = std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            out.sort();
            out
        };
        let mut out = Vec::new();
        for folder in folders {
            for path in apps_in(folder) {
                if path.extension().is_some_and(|e| e == "app") {
                    out.extend(one(&path));
                } else if path.is_dir() && !folders.contains(&path) {
                    for inner in apps_in(&path).into_iter().filter(|p| p.extension().is_some_and(|e| e == "app")) {
                        out.extend(one(&inner));
                    }
                }
            }
        }
        out
    }

    fn one(path: &Path) -> Option<InstalledApp> {
        let info = std::fs::read(path.join("Contents/Info.plist")).ok()?;
        bundle::app(path, &info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_entries_keep_applications_and_drop_parts_and_store_games() {
        let entry = |key: &str, name: &str| uninstall::Entry {
            key: key.into(),
            display_name: Some(name.into()),
            display_version: Some("1.2".into()),
            install_location: Some(r"C:\Program Files\Thing".into()),
            ..Default::default()
        };
        let app = uninstall::app(&entry("{ABC-1}", "Thing")).unwrap();
        assert_eq!(app.key(), "app:win:ABC-1");
        assert_eq!(app.version.as_deref(), Some("1.2"));
        assert!(uninstall::app(&entry("Steam App 440", "Steam App 440")).is_none());
        assert!(uninstall::app(&entry("x", "Microsoft Visual C++ 2015-2022 Redistributable (x64)")).is_none());
        assert!(uninstall::app(&uninstall::Entry { system_component: true, ..entry("y", "Hidden") }).is_none());
        assert!(uninstall::app(&uninstall::Entry { parent_key_name: Some("Office".into()), ..entry("z", "Part") }).is_none());
        assert!(uninstall::app(&uninstall::Entry { release_type: Some("Security Update".into()), ..entry("u", "KB1") }).is_none());
        assert!(uninstall::app(&uninstall::Entry { display_name: None, ..entry("n", "") }).is_none());
    }

    #[test]
    fn store_manifests_keep_listed_applications_only() {
        let manifest = |extra: &str, entry: &str| {
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10" xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10">
  <Identity Name="Vendor.Paint" Publisher="CN=Vendor" Version="2.3.4.0" />
  <Properties><DisplayName>Paint Thing</DisplayName><PublisherDisplayName>Vendor</PublisherDisplayName>{extra}</Properties>
  <Applications><Application Id="App"><uap:VisualElements DisplayName="Paint Thing" {entry}/></Application></Applications>
</Package>"#
            )
        };
        let app = msix::app(&manifest("", ""), PathBuf::from("C:/x")).unwrap();
        assert_eq!(app.key(), "app:msix:Vendor.Paint");
        assert_eq!(app.name, "Paint Thing");
        assert_eq!(app.version.as_deref(), Some("2.3.4.0"));
        assert!(msix::app(&manifest("<Framework>true</Framework>", ""), PathBuf::new()).is_none());
        assert!(msix::app(&manifest("", r#"AppListEntry="none""#), PathBuf::new()).is_none());
    }

    #[test]
    fn desktop_entries_name_their_source_and_skip_hidden_ones_and_game_shortcuts() {
        let gimp = "[Desktop Entry]\nType=Application\nName=GNU Image Manipulation Program\nName[de]=GIMP\nExec=/usr/bin/flatpak run org.gimp.GIMP\nX-Flatpak=org.gimp.GIMP\n\n[Desktop Action new]\nName=New Window\n";
        let app = desktop::app("org.gimp.GIMP", gimp, false).unwrap();
        assert_eq!(app.key(), "app:flatpak:org.gimp.GIMP");
        assert_eq!(app.name, "GNU Image Manipulation Program");
        let plain = "[Desktop Entry]\nType=Application\nName=Calculator\nExec=gnome-calculator\n";
        assert_eq!(desktop::app("org.gnome.Calculator", plain, false).unwrap().key(), "app:desktop:org.gnome.Calculator");
        assert_eq!(desktop::app("firefox_firefox", plain, true).unwrap().key(), "app:snap:firefox");
        assert!(desktop::app("x", "[Desktop Entry]\nType=Application\nName=X\nNoDisplay=true\n", false).is_none());
        assert!(desktop::app("x", "[Desktop Entry]\nType=Link\nName=X\n", false).is_none());
        assert!(desktop::app("x", "[Desktop Entry]\nType=Application\nName=Portal\nExec=steam steam://rungameid/400\n", false).is_none());
    }

    #[test]
    fn desktop_files_found_first_win() {
        let dir = std::env::temp_dir().join(format!("dtagent-desktop-{}", std::process::id()));
        let (a, b) = (dir.join("a/applications"), dir.join("b/applications"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(b.join("kde")).unwrap();
        std::fs::write(a.join("calc.desktop"), "[Desktop Entry]\nType=Application\nName=Mine\n").unwrap();
        std::fs::write(b.join("calc.desktop"), "[Desktop Entry]\nType=Application\nName=System\n").unwrap();
        std::fs::write(b.join("kde/edit.desktop"), "[Desktop Entry]\nType=Application\nName=Editor\n").unwrap();
        let apps = linux::entries(&[(a, false), (b, false)]);
        let names: Vec<(String, String)> = apps.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
        assert_eq!(names, vec![("calc".to_string(), "Mine".to_string()), ("kde-edit".to_string(), "Editor".to_string())]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bundles_read_their_info_plist() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Blender</string>
<key>CFBundleIdentifier</key><string>org.blenderfoundation.blender</string>
<key>CFBundleShortVersionString</key><string>4.2.1</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#;
        let app = bundle::app(Path::new("/Applications/Blender.app"), xml).unwrap();
        assert_eq!(app.key(), "app:mac:org.blenderfoundation.blender");
        assert_eq!(app.version.as_deref(), Some("4.2.1"));
        let dir = std::env::temp_dir().join(format!("dtagent-mac-{}", std::process::id()));
        let contents = dir.join("Apps/Tools/Blender.app/Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(contents.join("Info.plist"), xml).unwrap();
        let found = mac::bundles(&[dir.join("Apps")]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Blender");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apps_inside_a_game_folder_are_the_game() {
        let games = vec![PathBuf::from(r"C:\Games\Portal"), PathBuf::from("/home/me/Games/Celeste/")];
        assert!(inside_a_game(Path::new(r"C:\games\portal\bin"), &games));
        assert!(inside_a_game(Path::new("/home/me/Games/Celeste"), &games));
        assert!(!inside_a_game(Path::new(r"C:\Games\Portal 2"), &games));
    }
}
