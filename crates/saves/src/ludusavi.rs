//! Save locations from the Ludusavi manifest (mtkennerly/ludusavi-manifest,
//! its data drawn from PCGamingWiki). It is fetched by the agent when the
//! person asks (`droidtop-agent manifest update`) or the first time a save
//! lookup needs it, never bundled. The YAML is read once and kept as a
//! compact JSON index of each game's Windows save templates, by Steam id,
//! GOG id, title and install folder name.
//!
//! Only rules that apply on Windows are kept: the handheld runs the Windows
//! build in Wine, so those are the saves both sides have.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use droidtop_agent_core::library::title_key;
use droidtop_agent_core::saves::{self, SaveSpec};
use serde::{Deserialize, Serialize};

pub const MANIFEST_URL: &str = "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Index {
    /// Manifest name to its templates.
    pub games: BTreeMap<String, Vec<String>>,
    pub steam: BTreeMap<String, String>,
    pub gog: BTreeMap<String, String>,
    pub titles: BTreeMap<String, String>,
    pub dirs: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
struct Game {
    #[serde(default)]
    files: BTreeMap<String, Option<FileRule>>,
    #[serde(default)]
    steam: Option<StoreId>,
    #[serde(default)]
    gog: Option<StoreId>,
    #[serde(default, rename = "installDir")]
    install_dir: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Deserialize, Default)]
struct FileRule {
    #[serde(default)]
    when: Vec<When>,
}

#[derive(Deserialize)]
struct When {
    #[serde(default)]
    os: Option<String>,
}

#[derive(Deserialize)]
struct StoreId {
    #[serde(default)]
    id: Option<u64>,
}

/// A Ludusavi template in the tokens both sides resolve, or None when it
/// cannot be synced (a registry path, a fixed drive, a token only one side has).
pub fn convert(template: &str) -> Option<String> {
    let t = template.replace('\\', "/").replace("<root>/<game>", "<base>");
    if t.contains("<root>") || t.contains("<game>") {
        return None;
    }
    let t = t.replace("<storeUserId>", "*").replace("<osUserName>", "*").replace("<storeGameId>", "*");
    saves::split(&t).map(|_| t)
}

fn applies_on_windows(rule: &Option<FileRule>) -> bool {
    match rule {
        None => true,
        Some(r) => r.when.is_empty() || r.when.iter().any(|w| w.os.as_deref().is_none_or(|os| os == "windows")),
    }
}

/// Builds the index from the manifest's YAML.
pub fn index_from_yaml(reader: impl Read) -> Result<Index, String> {
    let manifest: BTreeMap<String, Game> = serde_yaml::from_reader(reader).map_err(|e| format!("the manifest could not be read: {e}"))?;
    let mut index = Index::default();
    for (name, game) in manifest {
        let patterns: Vec<String> =
            game.files.iter().filter(|(_, rule)| applies_on_windows(rule)).filter_map(|(t, _)| convert(t)).collect();
        if patterns.is_empty() {
            continue;
        }
        if let Some(id) = game.steam.and_then(|s| s.id) {
            index.steam.insert(id.to_string(), name.clone());
        }
        if let Some(id) = game.gog.and_then(|s| s.id) {
            index.gog.insert(id.to_string(), name.clone());
        }
        for dir in game.install_dir.keys() {
            index.dirs.insert(dir.to_lowercase(), name.clone());
        }
        index.titles.insert(title_key(&name), name.clone());
        index.games.insert(name, patterns);
    }
    Ok(index)
}

fn index_path(data: &Path) -> PathBuf {
    data.join("ludusavi-index.json")
}

pub fn load(data: &Path) -> Option<Index> {
    serde_json::from_slice(&fs::read(index_path(data)).ok()?).ok()
}

/// Fetches the manifest and replaces the index.
pub fn update(data: &Path) -> Result<Index, String> {
    let response = ureq::get(MANIFEST_URL).call().map_err(|e| format!("the Ludusavi manifest could not be fetched: {e}"))?;
    let index = index_from_yaml(response.into_reader())?;
    let path = index_path(data);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(&index).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(index)
}

impl Index {
    /// The manifest's name for a game: by store id, then title, then folder name.
    pub fn name_for(&self, key: &str, title: &str, folder: Option<&str>) -> Option<&String> {
        let by_store = match key.split_once(':') {
            Some(("steam", id)) => self.steam.get(id),
            Some(("gog", id)) => self.gog.get(id),
            _ => None,
        };
        by_store.or_else(|| self.titles.get(&title_key(title))).or_else(|| folder.and_then(|f| self.dirs.get(&f.to_lowercase())))
    }

    pub fn spec_for(&self, key: &str, title: &str, folder: Option<&str>) -> Option<SaveSpec> {
        let name = self.name_for(key, title, folder)?;
        Some(SaveSpec { patterns: self.games.get(name)?.clone() })
    }
}

/// The person's own custom games in Ludusavi's settings, by title key.
pub fn custom_games(config: &Path) -> BTreeMap<String, Vec<String>> {
    #[derive(Deserialize)]
    struct Config {
        #[serde(default, rename = "customGames")]
        custom_games: Vec<Custom>,
    }
    #[derive(Deserialize)]
    struct Custom {
        name: String,
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        ignore: bool,
    }
    let Ok(text) = fs::read_to_string(config) else { return BTreeMap::new() };
    let Ok(parsed) = serde_yaml::from_str::<Config>(&text) else { return BTreeMap::new() };
    parsed
        .custom_games
        .into_iter()
        .filter(|c| !c.ignore)
        .map(|c| (title_key(&c.name), c.files.iter().filter_map(|f| convert(f)).collect::<Vec<_>>()))
        .filter(|(_, p)| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
Some Game:
  files:
    <winAppData>/Some Game/Saves:
      tags: [save]
    <root>/<game>/save*.dat:
      when:
        - os: windows
    <home>/.local/share/somegame:
      when:
        - os: linux
    C:/fixed/path: {}
  installDir:
    Some Game Folder: {}
  steam:
    id: 440
Alias Only:
  alias: Some Game
"#;

    #[test]
    fn the_manifest_becomes_windows_templates_by_id_title_and_folder() {
        let index = index_from_yaml(SAMPLE.as_bytes()).unwrap();
        let spec = index.spec_for("steam:440", "whatever", None).unwrap();
        assert_eq!(spec.patterns, vec!["<base>/save*.dat".to_string(), "<winAppData>/Some Game/Saves".to_string()]);
        assert!(index.spec_for("epic:x", "some game", None).is_some());
        assert!(index.spec_for("epic:x", "nope", Some("SOME GAME FOLDER")).is_some());
        assert!(index.spec_for("epic:x", "Alias Only", None).is_none());
    }

    #[test]
    fn templates_convert_or_are_dropped() {
        assert_eq!(convert("<winDocuments>/My Games/<storeUserId>/x").unwrap(), "<winDocuments>/My Games/*/x");
        assert!(convert("<regHkcu>/Software/x").is_none());
        assert!(convert("<root>/other/<game>").is_none());
    }
}
