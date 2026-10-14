//! The window: one page per job, picked on the left. Everything slow (a
//! scan, pairing, fetching an adapter) runs on a thread of its own and the
//! page shows how it went; nothing on the drawing thread touches the network,
//! and files are read only when a page opens or the person asks.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droidtop_agent_computer::host::file_key;
use droidtop_agent_computer::scan::Found;
use droidtop_agent_computer::state::{Agent, Settings};
use droidtop_agent_computer::{autostart, pair, rendezvous, state};
use droidtop_agent_core::keys::{short, PeerId};
use droidtop_agent_core::library::now_ms;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::saves;
use droidtop_agent_core::savesync;
use eframe::egui::{self, Color32, RichText};

use crate::tray::{Action, Tray};

/// How the in-process service is doing.
#[derive(Debug, Clone)]
pub enum Service {
    Starting,
    Running,
    /// Another droidtop-agent on this computer is serving; this window shows
    /// and changes the state it keeps in the same files.
    Attached,
    Stopped(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Status,
    Pair,
    Library,
    Saves,
    Plugins,
    Settings,
}

impl Page {
    const ALL: [(Page, &'static str); 6] = [
        (Page::Status, "Status"),
        (Page::Pair, "Pair a handheld"),
        (Page::Library, "Library"),
        (Page::Saves, "Saves"),
        (Page::Plugins, "Plugin data"),
        (Page::Settings, "Settings"),
    ];
}

/// Pairing as the Pair page shows it.
#[derive(Default)]
enum Pairing {
    #[default]
    Idle,
    /// This computer shows a code and waits.
    Showing {
        code: String,
        places: Vec<pair::Place>,
        qr: Qr,
        until: Instant,
        cancel: Arc<AtomicBool>,
    },
    /// This computer is connecting with a code the handheld showed.
    Connecting,
    Paired(String),
    Failed(String),
}

/// A QR code's modules, row by row.
struct Qr {
    width: usize,
    dark: Vec<bool>,
}

impl Qr {
    fn of(text: &str) -> Option<Qr> {
        let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
        Some(Qr { width: code.width(), dark: code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect() })
    }
}

/// One kept copy of a game's saves.
#[derive(Clone)]
struct Kept {
    label: String,
    when: String,
    files: usize,
    path: PathBuf,
    /// The newest copy from a device, rather than an older one it replaced.
    newest: bool,
    /// The person said they had looked at it.
    reviewed: bool,
    /// What `reviewed.txt` calls it.
    name: String,
}

/// A game's kept copies; [`key`] is None when the library no longer has it.
#[derive(Clone)]
struct GameCopies {
    key: Option<String>,
    title: String,
    dir: PathBuf,
    copies: Vec<Kept>,
}

/// Where a game keeps its saves on this computer.
#[derive(Clone)]
struct SaveInfo {
    patterns: Vec<String>,
    /// The ones in [`patterns`] the person stated.
    custom: Vec<String>,
    files: Vec<(String, PathBuf)>,
}

/// The result of something running on a thread, shown on its page.
type Outcome = Arc<Mutex<Option<Result<String, String>>>>;

pub struct App {
    agent: Arc<Agent>,
    service: Arc<Mutex<Service>>,
    tray: Tray,
    page: Page,
    pairing: Arc<Mutex<Pairing>>,
    entered_code: String,
    library_filter: String,
    scanning: Arc<AtomicBool>,
    /// The game picked in the Library page, by key, and the key the collections draft is for.
    selected: Option<String>,
    draft_for: Option<String>,
    collections_draft: String,
    saves_filter: String,
    saves_game: Option<(String, String)>,
    save_info: Arc<Mutex<Option<SaveInfo>>>,
    template_draft: String,
    save_outcome: Outcome,
    archive: Vec<GameCopies>,
    rendezvous: Option<rendezvous::Status>,
    loaded_at: Instant,
    plugin_outcome: Outcome,
    settings: Option<Settings>,
    settings_outcome: Option<Result<String, String>>,
    autostart: bool,
    forget: Option<String>,
}

impl App {
    pub fn new(agent: Arc<Agent>, service: Arc<Mutex<Service>>, tray: Tray) -> App {
        App {
            agent,
            service,
            tray,
            page: Page::Status,
            pairing: Default::default(),
            entered_code: String::new(),
            library_filter: String::new(),
            scanning: Default::default(),
            selected: None,
            draft_for: None,
            collections_draft: String::new(),
            saves_filter: String::new(),
            saves_game: None,
            save_info: Default::default(),
            template_draft: String::new(),
            save_outcome: Default::default(),
            archive: Vec::new(),
            rendezvous: None,
            loaded_at: Instant::now() - Duration::from_secs(3600),
            plugin_outcome: Default::default(),
            settings: None,
            settings_outcome: None,
            autostart: autostart::enabled(),
            forget: None,
        }
    }

    fn open(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.loaded_at = Instant::now() - Duration::from_secs(3600);
            self.settings = None;
            self.settings_outcome = None;
        }
    }

    /// Reads what a page shows from files, when it opens and every few seconds.
    fn reload(&mut self) {
        if self.loaded_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.loaded_at = Instant::now();
        match self.page {
            Page::Status => self.rendezvous = state::read_json::<rendezvous::Status>(&self.agent.dirs.rendezvous()).ok(),
            Page::Saves => {
                let titles: std::collections::BTreeMap<String, (String, String)> =
                    self.agent.library_now().games.iter().map(|(k, g)| (file_key(k), (k.clone(), g.title.clone()))).collect();
                self.archive = archived(&self.agent.dirs.archive(), &titles);
            }
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(action) = self.tray.actions.try_recv() {
            match action {
                Action::Open => {}
                Action::Pair => self.open(Page::Pair),
                Action::Quit => std::process::exit(0),
            }
        }
        // Closing the window keeps the agent in the tray, when there is one.
        if ctx.input(|i| i.viewport().close_requested()) && self.tray.shown {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        self.reload();

        egui::SidePanel::left("pages").resizable(false).exact_width(170.0).show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("droidtop-agent");
            ui.label(RichText::new(self.agent.name()).weak());
            ui.add_space(12.0);
            for (page, label) in Page::ALL {
                let badge = match page {
                    Page::Plugins => self.agent.adapter_offers().len(),
                    _ => 0,
                };
                let text = if badge > 0 { format!("{label}  ({badge})") } else { label.to_string() };
                if ui.selectable_label(self.page == page, text).clicked() {
                    self.open(page);
                }
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.page {
                Page::Status => self.status(ui),
                Page::Pair => self.pair(ui),
                Page::Library => self.library(ui),
                Page::Saves => self.saves(ui),
                Page::Plugins => self.plugins(ui),
                Page::Settings => self.settings_page(ui),
            });
        });
        // Threads finish work the page shows; look again now and then.
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

fn ago(ms: i64) -> String {
    if ms <= 0 {
        return "never".into();
    }
    let s = ((now_ms() - ms) / 1000).max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn outcome_label(ui: &mut egui::Ui, outcome: &Result<String, String>) {
    match outcome {
        Ok(text) => ui.label(text),
        Err(text) => ui.colored_label(ui.visuals().error_fg_color, text),
    };
}

/// Opens a folder in the system's file manager.
fn open_folder(path: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

/// "2026-10-08 22:45 UTC" for a stamp such as `20261008T224500Z`.
fn pretty_stamp(stamp: &str) -> String {
    match (stamp.get(0..4), stamp.get(4..6), stamp.get(6..8), stamp.get(9..11), stamp.get(11..13)) {
        (Some(y), Some(mo), Some(d), Some(h), Some(mi)) => format!("{y}-{mo}-{d} {h}:{mi} UTC"),
        _ => stamp.to_string(),
    }
}

/// A launcher or store as the person knows it.
fn source_name(raw: &str) -> String {
    let key: String = raw.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    match key.as_str() {
        "" => String::new(),
        "folder" | "gamefolders" => "Game folder".into(),
        "romfolders" => "ROM folder".into(),
        "steam" => "Steam".into(),
        "gog" => "GOG".into(),
        "epic" => "Epic".into(),
        "amazon" => "Amazon Games".into(),
        "itch" => "itch.io".into(),
        "battlenet" => "Battle.net".into(),
        "heroic" => "Heroic".into(),
        "lutris" => "Lutris".into(),
        "bottlesandminigalaxy" => "Bottles and Minigalaxy".into(),
        _ => {
            let mut chars = raw.chars();
            chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
    }
}

/// The folders a source's games sit in, a few of them.
fn places_of(scan: &droidtop_agent_computer::scan::Scan, source: &str) -> String {
    let wanted: String = source.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let mut places: Vec<String> = scan
        .games
        .iter()
        .filter(|f| f.game.install.launcher.as_deref().is_some_and(|l| l == wanted))
        .filter_map(|f| f.base.as_ref().and_then(|b| b.parent()).map(|p| p.display().to_string()))
        .collect();
    places.sort();
    places.dedup();
    let more = places.len().saturating_sub(2);
    places.truncate(2);
    let mut out = places.join("; ");
    if more > 0 {
        out.push_str(&format!(" and {more} more"));
    }
    out
}

/// The archive: per game, the newest copy kept from each device
/// (`copies/<device>/`), then the older ones, newest first. [`titles`] names
/// each archive folder's game (`host::file_key` of its key, then key and title).
fn archived(root: &Path, titles: &std::collections::BTreeMap<String, (String, String)>) -> Vec<GameCopies> {
    let mut out = Vec::new();
    let mut games: Vec<PathBuf> =
        std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    games.sort();
    for dir in games {
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let reviewed: Vec<String> =
            std::fs::read_to_string(dir.join("reviewed.txt")).unwrap_or_default().lines().map(str::to_string).collect();
        let mut copies = Vec::new();
        let mut newest: Vec<PathBuf> = std::fs::read_dir(dir.join("copies")).into_iter().flatten().flatten().map(|e| e.path()).collect();
        newest.sort();
        for copy in newest {
            let device = copy.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let when =
                std::fs::metadata(&copy).and_then(|m| m.modified()).map(|t| pretty_stamp(&savesync::utc_stamp(t))).unwrap_or_default();
            copies.push(Kept {
                label: format!("Newest from {device}"),
                when,
                files: walk_count(&copy, 0),
                path: copy,
                newest: true,
                reviewed: true,
                name: device,
            });
        }
        for older in savesync::archived(&dir).into_iter().rev() {
            let stamp = older.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let device = std::fs::read_to_string(older.join("device.txt")).unwrap_or_default();
            let files = walk_count(&older, 0).saturating_sub(usize::from(!device.is_empty()));
            copies.push(Kept {
                label: format!("Older, from {}", device.trim()),
                when: pretty_stamp(&stamp),
                files,
                path: older,
                newest: false,
                reviewed: reviewed.contains(&stamp),
                name: stamp,
            });
        }
        if copies.is_empty() {
            continue;
        }
        let (key, title) = match titles.get(&folder) {
            Some((key, title)) => (Some(key.clone()), title.clone()),
            None => (None, "A game that is no longer in the library".to_string()),
        };
        out.push(GameCopies { key, title, dir, copies });
    }
    out
}

fn walk_count(dir: &Path, depth: usize) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| if e.path().is_dir() && depth < 8 { walk_count(&e.path(), depth + 1) } else { 1 })
        .sum()
}

impl App {
    fn status(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status");
        ui.add_space(6.0);
        let service = self.service.lock().unwrap().clone();
        match service {
            Service::Starting => ui.label("Starting…"),
            Service::Running => ui.label(format!(
                "Serving paired handhelds on this network (port {}). The handheld connects when a game starts or ends, or when you sync.",
                droidtop_agent_core::PORT
            )),
            Service::Attached => ui.label(
                "Serving paired handhelds: droidtop-agent is already running on this computer. This window shows what it keeps and changes the same files.",
            ),
            Service::Stopped(why) => ui.colored_label(ui.visuals().error_fg_color, why),
        };
        ui.label(
            RichText::new(
                "If your firewall asks whether droidtop-agent may use the network, allow it on your home network: handhelds reach it on TCP and UDP 47610, and on UDP 47611 away from home.",
            )
            .weak(),
        );
        ui.add_space(12.0);
        ui.strong("Paired handhelds");
        let devices = self.agent.devices_now();
        if devices.is_empty() {
            ui.label("None yet.");
            if ui.button("Pair a handheld").clicked() {
                self.open(Page::Pair);
            }
        }
        let mut forget_now = None;
        egui::Grid::new("devices").striped(true).num_columns(4).show(ui, |ui| {
            for d in &devices {
                ui.label(&d.name);
                ui.label(format!("seen {}", ago(d.last_seen_ms)));
                ui.label(RichText::new(d.last_address.clone().unwrap_or_default()).weak());
                if self.forget.as_deref() == Some(&d.id) {
                    if ui.button("Forget it: it can no longer sync").clicked() {
                        forget_now = Some(d.id.clone());
                    }
                } else if ui.button("Forget…").clicked() {
                    self.forget = Some(d.id.clone());
                }
                ui.end_row();
            }
        });
        if let Some(id) = forget_now {
            if let Ok(peer) = PeerId::from_hex(&id) {
                let _ = self.agent.remove_device(&peer);
            }
            self.forget = None;
        }
        ui.add_space(12.0);
        ui.strong("Addresses handhelds can use");
        ui.label(
            RichText::new("On your network or a VPN, a handheld needs this address and the port. Pair with the first one that works.")
                .weak(),
        );
        egui::Grid::new("addresses").striped(true).num_columns(2).show(ui, |ui| {
            for place in pair::places(droidtop_agent_core::PORT) {
                ui.label(RichText::new(&place.address).monospace());
                ui.label(RichText::new(place.label).weak());
                ui.end_row();
            }
            // The address the internet sees comes last: it only works from outside, through a forwarded port.
            if let Some(mapped) = self.rendezvous.as_ref().and_then(|st| st.mapped.clone()) {
                ui.label(RichText::new(mapped).monospace());
                ui.label(RichText::new("from outside your network").weak());
                ui.end_row();
            }
        });
        ui.add_space(12.0);
        ui.strong("Away from home");
        let settings = self.agent.settings.lock().unwrap().clone();
        if !settings.rendezvous {
            ui.label(
                "Off: paired handhelds reach this computer only on this network, through a forwarded port, or through the cloud folder.",
            );
        } else {
            match &self.rendezvous {
                Some(st) => {
                    if st.mapped.is_some() {
                        ui.label("Handhelds find this computer from outside through global discovery (its address is listed above).");
                    } else {
                        ui.label("Looking for this computer's address on the internet…");
                    }
                    if let Some(problem) = &st.last_problem {
                        ui.colored_label(ui.visuals().warn_fg_color, problem);
                    }
                }
                None => {
                    ui.label("Starting…");
                }
            }
        }
        if let Some(at) = &settings.public_endpoint {
            ui.label(format!("Forwarded port: {at}"));
        }
        if let Some(share) = &settings.share {
            ui.label(format!("Cloud folder: {}", share.display()));
        }
        ui.add_space(12.0);
        ui.label(RichText::new(format!("This computer's id: {}", short(&self.agent.peer_id()))).weak());
    }

    fn pair(&mut self, ui: &mut egui::Ui) {
        ui.heading("Pair a handheld");
        ui.add_space(6.0);
        let mut pairing = self.pairing.lock().unwrap();
        match &*pairing {
            Pairing::Showing { code, places, qr, until, .. } => {
                ui.label("On the handheld: Settings > Computers > Pair a computer > Use a code from the computer, and type:");
                ui.add_space(6.0);
                egui::Grid::new("showing").num_columns(2).show(ui, |ui| {
                    ui.label("Address");
                    ui.vertical(|ui| {
                        for place in places {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&place.address).size(20.0).monospace());
                                ui.label(RichText::new(&place.label).weak());
                            });
                        }
                    });
                    ui.end_row();
                    ui.label("Code");
                    ui.label(RichText::new(format!("{} {}", &code[..3], &code[3..])).size(36.0).monospace().strong());
                    ui.end_row();
                });
                let left = until.saturating_duration_since(Instant::now()).as_secs();
                ui.label(RichText::new(format!("The code works for {} more minutes.", left / 60 + 1)).weak());
                ui.add_space(8.0);
                draw_qr(ui, qr, 4.0);
                ui.label(RichText::new("The same invitation as a QR code, for a device with a camera.").weak());
                if ui.button("Stop").clicked() {
                    if let Pairing::Showing { cancel, .. } = &*pairing {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                return;
            }
            Pairing::Connecting => {
                ui.label("Pairing…");
                ui.spinner();
                return;
            }
            Pairing::Paired(name) => {
                ui.label(RichText::new(format!("Paired with {name}.")).strong());
                ui.label("It syncs when a game starts or ends, or when you sync from the handheld.");
            }
            Pairing::Failed(why) => {
                ui.colored_label(ui.visuals().error_fg_color, why);
            }
            Pairing::Idle => {}
        }
        ui.add_space(8.0);
        ui.strong("The handheld shows a code");
        ui.label("On the handheld: Settings > Computers > Pair a computer. Type the 6 digits it shows (or the text of its QR code) here.");
        let mut start_with_code = false;
        ui.horizontal(|ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut self.entered_code).hint_text("123 456").desired_width(160.0));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add_enabled(!self.entered_code.trim().is_empty(), egui::Button::new("Pair")).clicked() || enter {
                start_with_code = true;
            }
        });
        ui.add_space(12.0);
        ui.strong("This computer shows a code");
        ui.label("For a handheld this computer cannot reach by itself, such as one in an emulator or on a guest network.");
        let show = ui.button("Show a code").clicked();
        if start_with_code {
            *pairing = Pairing::Connecting;
            let (agent, state, text) = (self.agent.clone(), self.pairing.clone(), self.entered_code.trim().to_string());
            std::thread::spawn(move || {
                let result = pair::pair(&agent, &text);
                *state.lock().unwrap() = match result {
                    Ok(name) => Pairing::Paired(name),
                    Err(e) => Pairing::Failed(e),
                };
            });
            self.entered_code.clear();
        } else if show {
            match pair::listen() {
                Ok(showing) => {
                    let cancel = Arc::new(AtomicBool::new(false));
                    let qr = Qr::of(&showing.invite(&self.agent)).unwrap_or(Qr { width: 0, dark: Vec::new() });
                    *pairing = Pairing::Showing {
                        code: showing.code.clone(),
                        places: showing.places.clone(),
                        qr,
                        until: showing.until,
                        cancel: cancel.clone(),
                    };
                    let (agent, state) = (self.agent.clone(), self.pairing.clone());
                    std::thread::spawn(move || {
                        let result = pair::wait(&agent, showing, &cancel);
                        *state.lock().unwrap() = match result {
                            Ok(name) => Pairing::Paired(name),
                            Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => Pairing::Idle,
                            Err(e) => Pairing::Failed(e),
                        };
                    });
                }
                Err(e) => *pairing = Pairing::Failed(e),
            }
        }
    }

    fn rescan_in_background(&self) {
        if self.scanning.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let (agent, flag) = (self.agent.clone(), self.scanning.clone());
        std::thread::spawn(move || {
            agent.rescan();
            flag.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    }

    /// Saves a change to the settings and looks again at what the computer has.
    fn changed_settings(&self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.agent.settings.lock().unwrap());
        let _ = self.agent.save_settings();
        self.rescan_in_background();
    }

    fn library(&mut self, ui: &mut egui::Ui) {
        ui.heading("Library");
        ui.add_space(6.0);
        let scanning = self.scanning.load(std::sync::atomic::Ordering::Relaxed);
        ui.horizontal(|ui| {
            if ui.add_enabled(!scanning, egui::Button::new(if scanning { "Looking…" } else { "Look again" })).clicked() {
                self.rescan_in_background();
            }
            ui.label(RichText::new("The games on this computer, as droidtop on your handhelds sees them.").weak());
        });
        let settings = self.agent.settings.lock().unwrap().clone();
        let guard = self.agent.scan.lock().unwrap();
        let Some((at, scan)) = guard.as_ref() else {
            ui.label("Looking for games…");
            return;
        };
        ui.label(
            RichText::new(format!(
                "Last looked {} min ago; it looks again by itself every {} minutes.",
                at.elapsed().as_secs() / 60,
                settings.scan_minutes
            ))
            .weak(),
        );
        let mut switch_source: Option<(String, bool)> = None;
        let mut switch_folder: Option<(PathBuf, bool)> = None;
        let mut remove_folder: Option<(PathBuf, bool)> = None;
        let mut add_folder: Option<bool> = None;

        ui.add_space(10.0);
        ui.strong("Where games are found");
        ui.label(RichText::new("Switch a place off and its games leave the library on your handhelds too.").weak());
        egui::Grid::new("sources").striped(true).num_columns(3).show(ui, |ui| {
            for s in scan.sources.iter().filter(|s| s.name != "game folders" && s.name != "ROM folders") {
                // A store this computer does not have is not worth a row.
                if s.enabled && s.count == 0 {
                    continue;
                }
                let mut on = s.enabled;
                if ui.checkbox(&mut on, source_name(&s.name)).changed() {
                    switch_source = Some((s.name.clone(), on));
                }
                ui.label(RichText::new(places_of(scan, &s.name)).weak().small());
                ui.label(if s.enabled { format!("{} games", s.count) } else { "off".into() });
                ui.end_row();
            }
        });
        for (title, roms) in [("Game folders", false), ("ROM folders (laid out by ES-DE system name)", true)] {
            ui.add_space(8.0);
            ui.strong(title);
            let rows: Vec<(PathBuf, bool)> = if roms {
                settings.rom_folders.iter().map(|p| (p.clone(), false)).collect()
            } else {
                droidtop_agent_computer::scan::folders::all_roots(&settings)
            };
            if rows.is_empty() {
                ui.label(RichText::new("None yet.").weak());
            }
            egui::Grid::new(title).striped(true).num_columns(4).show(ui, |ui| {
                for (path, auto) in &rows {
                    let enabled = !settings.disabled_folders.iter().any(|d| d == path);
                    let mut on = enabled;
                    if ui.checkbox(&mut on, path.display().to_string()).changed() {
                        switch_folder = Some((path.clone(), on));
                    }
                    let count = scan
                        .games
                        .iter()
                        .filter(|f| f.base.as_ref().is_some_and(|b| b.starts_with(path)) && f.game.key.starts_with("rom:") == roms)
                        .count();
                    ui.label(if enabled { format!("{count} games") } else { "off".into() });
                    ui.label(RichText::new(if *auto { "found on its own" } else { "added by you" }).weak().small());
                    if !auto && ui.small_button("Remove").clicked() {
                        remove_folder = Some((path.clone(), roms));
                    }
                    ui.end_row();
                }
            });
            if ui.button(if roms { "Add a ROM folder…" } else { "Add a game folder…" }).clicked() {
                add_folder = Some(roms);
            }
        }

        ui.add_space(12.0);
        ui.strong(format!("Games ({})", scan.games.len()));
        ui.add(egui::TextEdit::singleline(&mut self.library_filter).hint_text("Search").desired_width(260.0));
        let filter = self.library_filter.to_lowercase();
        let mut picked: Option<String> = None;
        egui::ScrollArea::vertical().id_salt("games").max_height(260.0).show(ui, |ui| {
            egui::Grid::new("games").striped(true).num_columns(3).show(ui, |ui| {
                for f in scan.games.iter().filter(|f| filter.is_empty() || f.game.title.to_lowercase().contains(&filter)) {
                    let chosen = self.selected.as_deref() == Some(f.game.key.as_str());
                    if ui.selectable_label(chosen, &f.game.title).clicked() {
                        picked = Some(f.game.key.clone());
                    }
                    ui.label(RichText::new(source_name(f.game.install.launcher.as_deref().unwrap_or(""))).weak());
                    ui.label(RichText::new(f.game.install.path.clone().unwrap_or_default()).weak().small());
                    ui.end_row();
                }
            });
        });
        let selected_game = self.selected.as_ref().and_then(|k| scan.games.iter().find(|f| &f.game.key == k)).cloned();
        let apps = scan.apps.len();
        let programs: Vec<String> = scan.programs.iter().map(|p| format!("{}: {}", p.name, p.note)).collect();
        let problems = scan.problems.clone();
        drop(guard);

        if let Some(key) = picked {
            self.selected = Some(key);
            self.draft_for = None;
        }
        if let Some(game) = selected_game {
            self.game_marks(ui, &game);
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Programs droidtop can sync with ({})", programs.len())).show(ui, |ui| {
            for p in &programs {
                ui.label(p);
            }
        });
        ui.label(
            RichText::new(format!("{apps} other programs are installed here; they show in Apps on a handheld that asks for them."))
                .weak()
                .small(),
        );
        if !problems.is_empty() {
            ui.add_space(8.0);
            ui.strong("Could not read");
            for p in &problems {
                ui.colored_label(ui.visuals().warn_fg_color, p);
            }
        }

        if let Some((name, on)) = switch_source {
            self.changed_settings(|s| {
                s.disabled_sources.retain(|d| *d != name);
                if !on {
                    s.disabled_sources.push(name);
                }
            });
        }
        if let Some((path, on)) = switch_folder {
            self.changed_settings(|s| {
                s.disabled_folders.retain(|d| *d != path);
                if !on {
                    s.disabled_folders.push(path);
                }
            });
        }
        if let Some((path, roms)) = remove_folder {
            self.changed_settings(|s| {
                let list = if roms { &mut s.rom_folders } else { &mut s.game_folders };
                list.retain(|f| *f != path);
                s.disabled_folders.retain(|d| *d != path);
            });
        }
        if let Some(roms) = add_folder {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                self.changed_settings(|s| {
                    let list = if roms { &mut s.rom_folders } else { &mut s.game_folders };
                    if !list.contains(&folder) {
                        list.push(folder);
                    }
                });
            }
        }
    }

    /// One game's details and the person's own marks on it, which travel to
    /// every paired handheld like marks made there.
    fn game_marks(&mut self, ui: &mut egui::Ui, game: &Found) {
        let key = game.game.key.clone();
        let marks = self.agent.library_now().games.get(&key).map(|r| r.marks.clone()).unwrap_or_default();
        let flag = |field: &str| marks.get(field).and_then(|m| m.value.as_bool()).unwrap_or(false);
        if self.draft_for.as_deref() != Some(key.as_str()) {
            let names: Vec<String> = marks
                .get("collections")
                .and_then(|m| m.value.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            self.collections_draft = names.join(", ");
            self.draft_for = Some(key.clone());
        }
        ui.add_space(8.0);
        ui.group(|ui| {
            ui.strong(&game.game.title);
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    source_name(game.game.install.launcher.as_deref().unwrap_or("")),
                    game.game.install.path.clone().unwrap_or_default()
                ))
                .weak()
                .small(),
            );
            ui.label(RichText::new("Your marks on this game: handhelds get them the next time they sync.").weak().small());
            for (field, label) in [("favourite", "Favourite"), ("hidden", "Hidden")] {
                let mut on = flag(field);
                if ui.checkbox(&mut on, label).changed() {
                    self.agent.set_mark(&key, field, serde_json::Value::Bool(on));
                }
            }
            ui.horizontal(|ui| {
                ui.label("Collections");
                ui.add(egui::TextEdit::singleline(&mut self.collections_draft).hint_text("Done, Platformers").desired_width(240.0));
                if ui.button("Save").clicked() {
                    let mut names: Vec<String> =
                        self.collections_draft.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
                    names.sort();
                    names.dedup();
                    self.agent.set_mark(&key, "collections", serde_json::json!(names));
                }
            });
        });
    }

    fn saves(&mut self, ui: &mut egui::Ui) {
        ui.heading("Saves");
        ui.add_space(6.0);
        ui.label(
            "Saves are kept in step between this computer and your handhelds. When both changed a game's saves, the newest copy wins and the other is kept here, so nothing is lost.",
        );
        ui.add_space(8.0);
        let devices = self.agent.devices_now();
        let mut preferred = self.agent.settings.lock().unwrap().primary_device.clone();
        let before = preferred.clone();
        ui.horizontal(|ui| {
            ui.label("When both changed, prefer");
            let chosen = preferred
                .as_ref()
                .and_then(|id| devices.iter().find(|d| &d.id == id))
                .map(|d| format!("{}'s saves", d.name))
                .unwrap_or_else(|| "the newest saves".into());
            egui::ComboBox::from_id_salt("prefer").selected_text(chosen).show_ui(ui, |ui| {
                ui.selectable_value(&mut preferred, None, "the newest saves");
                for d in &devices {
                    ui.selectable_value(&mut preferred, Some(d.id.clone()), format!("{}'s saves", d.name));
                }
            });
        });
        if preferred != before {
            self.agent.settings.lock().unwrap().primary_device = preferred;
            let _ = self.agent.save_settings();
        }
        let outcome = self.save_outcome.lock().unwrap().clone();
        if let Some(outcome) = &outcome {
            ui.add_space(6.0);
            outcome_label(ui, outcome);
        }

        ui.add_space(10.0);
        let to_review = self.archive.iter().filter(|g| g.copies.iter().any(|c| !c.newest && !c.reviewed)).count();
        if to_review > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{to_review} games have an older copy kept after both sides changed. Look at them, then restore one or keep what is here."),
            );
        }
        ui.strong("Find a game");
        ui.add(egui::TextEdit::singleline(&mut self.saves_filter).hint_text("Search your games").desired_width(260.0));
        let filter = self.saves_filter.to_lowercase();
        let mut picked: Option<(String, String)> = None;
        {
            let guard = self.agent.scan.lock().unwrap();
            let kept: Vec<&String> = self.archive.iter().filter_map(|g| g.key.as_ref()).collect();
            let custom: Vec<String> = self.agent.saves.lock().unwrap().keys().cloned().collect();
            let mut shown = 0;
            ui.horizontal_wrapped(|ui| {
                if let Some((_, scan)) = guard.as_ref() {
                    for f in scan.games.iter() {
                        let interesting = kept.contains(&&f.game.key) || custom.contains(&f.game.key);
                        let matches = !filter.is_empty() && f.game.title.to_lowercase().contains(&filter);
                        if ((filter.is_empty() && interesting) || matches) && shown < 40 {
                            shown += 1;
                            let chosen = self.saves_game.as_ref().map(|(k, _)| k) == Some(&f.game.key);
                            if ui.selectable_label(chosen, &f.game.title).clicked() {
                                picked = Some((f.game.key.clone(), f.game.title.clone()));
                            }
                        }
                    }
                }
                if shown == 0 {
                    let hint = if filter.is_empty() { "Search for a game, or pick one that has kept copies." } else { "No game matches." };
                    ui.label(RichText::new(hint).weak());
                }
            });
        }
        if let Some((key, title)) = picked {
            self.pick_saves_game(key, title);
        }
        if let Some((key, title)) = self.saves_game.clone() {
            self.saves_detail(ui, &key, &title);
        }
        let orphans: Vec<&GameCopies> = self.archive.iter().filter(|g| g.key.is_none()).collect();
        if !orphans.is_empty() {
            ui.add_space(10.0);
            ui.strong("Kept copies of games that are no longer in the library");
            for g in orphans {
                ui.horizontal(|ui| {
                    ui.label(&g.title);
                    if ui.small_button("Open folder").clicked() {
                        open_folder(&g.dir);
                    }
                });
            }
        }
    }

    /// Looks up where a game's saves are, off the drawing thread.
    fn pick_saves_game(&mut self, key: String, title: String) {
        self.saves_game = Some((key.clone(), title.clone()));
        let info: Arc<Mutex<Option<SaveInfo>>> = Default::default();
        self.save_info = info.clone();
        let agent = self.agent.clone();
        std::thread::spawn(move || {
            let game = GameRef { key: key.clone(), title };
            let custom = agent.saves.lock().unwrap().get(&key).map(|u| u.patterns.clone()).unwrap_or_default();
            let (patterns, files) = match agent.save_lookup(&game, false) {
                Some((spec, roots, _)) => {
                    let files = saves::collect(&spec, &roots);
                    (spec.patterns, files)
                }
                None => (Vec::new(), Vec::new()),
            };
            *info.lock().unwrap() = Some(SaveInfo { patterns, custom, files });
        });
    }

    fn saves_detail(&mut self, ui: &mut egui::Ui, key: &str, title: &str) {
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.strong(title);
            let info = self.save_info.lock().unwrap().clone();
            ui.add_space(4.0);
            ui.label("Where its saves are");
            match &info {
                None => {
                    ui.label(RichText::new("Looking…").weak());
                }
                Some(info) if info.patterns.is_empty() => {
                    ui.label(RichText::new("Not known yet. Add the folder below and it will be synced.").weak());
                }
                Some(info) => {
                    for p in &info.patterns {
                        let how = if info.custom.contains(p) { "set by you" } else { "detected" };
                        ui.label(RichText::new(format!("{p}   ({how})")).monospace().small());
                    }
                    ui.label(RichText::new(format!("{} files found now.", info.files.len())).weak());
                    if let Some(folder) = info.files.first().and_then(|(_, p)| p.parent()) {
                        if ui.small_button("Open folder").clicked() {
                            open_folder(folder);
                        }
                    }
                }
            }
            let mut remove: Option<String> = None;
            if let Some(info) = &info {
                for p in &info.custom {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Yours: {p}")).monospace().small());
                        if ui.small_button("Remove").clicked() {
                            remove = Some(p.clone());
                        }
                    });
                }
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.template_draft).hint_text("<winDocuments>/My Games/Name").desired_width(300.0));
                if ui.add_enabled(!self.template_draft.trim().is_empty(), egui::Button::new("Add location")).clicked() {
                    let template = self.template_draft.trim().to_string();
                    let result = if saves::split(&template).is_none() {
                        Err(format!("Start with one of: {}", saves::TOKENS.join(", ")))
                    } else {
                        self.agent.saves.lock().unwrap().entry(key.to_string()).or_default().patterns.push(template);
                        self.agent.save_saves().map(|()| "Added.".to_string()).map_err(|e| e.to_string())
                    };
                    if result.is_ok() {
                        self.template_draft.clear();
                        self.pick_saves_game(key.to_string(), title.to_string());
                    }
                    *self.save_outcome.lock().unwrap() = Some(result);
                }
            });
            ui.label(
                RichText::new("A location starts with <winDocuments>, <winAppData>, <winLocalAppData> or <base> (the game's own folder).")
                    .weak()
                    .small(),
            );
            if let Some(p) = remove {
                if let Some(u) = self.agent.saves.lock().unwrap().get_mut(key) {
                    u.patterns.retain(|x| *x != p);
                }
                let _ = self.agent.save_saves();
                self.pick_saves_game(key.to_string(), title.to_string());
            }

            ui.add_space(8.0);
            ui.label("Copies kept on this computer");
            match self.archive.iter().find(|g| g.key.as_deref() == Some(key)).cloned() {
                None => {
                    ui.label(RichText::new("Nothing is kept yet: a copy is kept when a sync would overwrite saves that differ.").weak());
                }
                Some(game) => {
                    let mut restore: Option<PathBuf> = None;
                    egui::Grid::new("copies").striped(true).num_columns(5).show(ui, |ui| {
                        for c in &game.copies {
                            ui.label(&c.label);
                            ui.label(&c.when);
                            ui.label(format!("{} files", c.files));
                            ui.horizontal(|ui| {
                                if ui.small_button("Restore").clicked() {
                                    restore = Some(c.path.clone());
                                }
                                if ui.small_button("Open folder").clicked() {
                                    open_folder(&c.path);
                                }
                            });
                            if !c.newest && !c.reviewed {
                                ui.colored_label(ui.visuals().warn_fg_color, "to look at");
                            } else {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                    });
                    if game.copies.iter().any(|c| !c.newest && !c.reviewed) && ui.button("Keep what is here (I have looked)").clicked() {
                        let names: Vec<String> = game.copies.iter().filter(|c| !c.newest).map(|c| c.name.clone()).collect();
                        let _ = std::fs::write(game.dir.join("reviewed.txt"), names.join("\n"));
                        self.loaded_at = Instant::now() - Duration::from_secs(3600);
                    }
                    if let Some(copy) = restore {
                        let (agent, outcome) = (self.agent.clone(), self.save_outcome.clone());
                        let game_ref = GameRef { key: key.to_string(), title: title.to_string() };
                        std::thread::spawn(move || {
                            let result = agent
                                .restore_saves(&game_ref, &copy)
                                .map(|n| format!("Put {n} files back. What was here before is kept as a copy."));
                            *outcome.lock().unwrap() = Some(result);
                        });
                    }
                }
            }
        });
    }

    fn plugins(&mut self, ui: &mut egui::Ui) {
        ui.heading("Plugin data");
        ui.add_space(6.0);
        ui.label("Some droidtop plugins keep their data in step with a program on this computer, through a small adapter program the plugin publishes.");
        if let Some(outcome) = self.plugin_outcome.lock().unwrap().as_ref() {
            outcome_label(ui, outcome);
        }
        let offers = self.agent.adapter_offers();
        if !offers.is_empty() {
            ui.add_space(8.0);
            ui.strong("Waiting for you");
        }
        for (context, offer) in offers {
            ui.group(|ui| {
                let name = if offer.label.is_empty() { context.clone() } else { offer.label.clone() };
                ui.label(RichText::new(format!("{name}, offered by the plugin {}", offer.plugin)).strong());
                match offer.for_this_system() {
                    Some(program) => {
                        ui.label(RichText::new(format!("{}\nSHA-256 {}", program.url, program.sha256)).weak().small());
                        ui.horizontal(|ui| {
                            if ui.button("Install").clicked() {
                                let (agent, outcome, context) = (self.agent.clone(), self.plugin_outcome.clone(), context.clone());
                                *outcome.lock().unwrap() = Some(Ok(format!("Fetching the {context} adapter…")));
                                std::thread::spawn(move || {
                                    let result = agent.approve(&context);
                                    *outcome.lock().unwrap() = Some(result);
                                });
                            }
                            if ui.button("Decline").clicked() {
                                let result = self.agent.decline(&context).map(|()| format!("Declined the {context} adapter."));
                                *self.plugin_outcome.lock().unwrap() = Some(result);
                            }
                        });
                    }
                    None => {
                        ui.label("The plugin offers no program for this kind of computer.");
                    }
                }
            });
        }
        let adapters = self.agent.settings.lock().unwrap().adapters.clone();
        ui.add_space(8.0);
        ui.strong("Installed");
        if adapters.is_empty() {
            ui.label("None.");
        }
        for (context, program) in adapters {
            ui.horizontal(|ui| {
                ui.label(&context);
                ui.label(RichText::new(program.display().to_string()).weak().small());
                if ui.button("Remove").clicked() {
                    self.agent.settings.lock().unwrap().adapters.remove(&context);
                    let _ = self.agent.save_settings();
                }
            });
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(6.0);
        if ui.checkbox(&mut self.autostart, "Start droidtop-agent when I sign in").changed() {
            self.settings_outcome = Some(autostart::set(self.autostart));
            self.autostart = autostart::enabled();
        }
        let devices = self.agent.devices_now();
        let draft = self.settings.get_or_insert_with(|| self.agent.settings.lock().unwrap().clone());
        let mut name = draft.name.clone().unwrap_or_default();
        ui.add_space(8.0);
        egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("This computer's name");
            ui.add(egui::TextEdit::singleline(&mut name).hint_text(self.agent.name()));
            ui.end_row();
            ui.label("Rescan every");
            ui.add(egui::DragValue::new(&mut draft.scan_minutes).range(5..=1440).suffix(" min"));
            ui.end_row();
            ui.label("Primary handheld");
            let chosen = draft
                .primary_device
                .as_ref()
                .and_then(|id| devices.iter().find(|d| &d.id == id))
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "none: the newest saves win".into());
            egui::ComboBox::from_id_salt("primary").selected_text(chosen).show_ui(ui, |ui| {
                ui.selectable_value(&mut draft.primary_device, None, "none: the newest saves win");
                for d in &devices {
                    ui.selectable_value(&mut draft.primary_device, Some(d.id.clone()), format!("{}: its saves win", d.name));
                }
            });
            ui.end_row();
            ui.label("Away from home");
            ui.checkbox(&mut draft.rendezvous, "Let paired handhelds find this computer through global discovery");
            ui.end_row();
            ui.label("Forwarded port");
            let mut endpoint = draft.public_endpoint.clone().unwrap_or_default();
            ui.add(egui::TextEdit::singleline(&mut endpoint).hint_text("203.0.113.7:47611"));
            draft.public_endpoint = Some(endpoint.trim().to_string()).filter(|e| !e.is_empty());
            ui.end_row();
            ui.label("Cloud folder");
            ui.horizontal(|ui| {
                ui.label(draft.share.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()));
                if ui.button("Choose…").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        draft.share = Some(folder);
                    }
                }
                if draft.share.is_some() && ui.button("Clear").clicked() {
                    draft.share = None;
                }
            });
            ui.end_row();
        });
        draft.name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
        for (label, roms) in [("Game folders", false), ("ROM folders (laid out by ES-DE system name)", true)] {
            ui.add_space(8.0);
            ui.strong(label);
            let list = if roms { &mut draft.rom_folders } else { &mut draft.game_folders };
            let mut remove = None;
            for (i, folder) in list.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(folder.display().to_string());
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            if ui.button("Add a folder…").clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    if !list.contains(&folder) {
                        list.push(folder);
                    }
                }
            }
        }
        ui.add_space(12.0);
        let changed = *draft != *self.agent.settings.lock().unwrap();
        if ui.add_enabled(changed, egui::Button::new("Save")).clicked() {
            let valid = draft.public_endpoint.as_deref().is_none_or(|e| e.parse::<std::net::SocketAddr>().is_ok());
            self.settings_outcome = Some(if !valid {
                Err("The forwarded port is an address and port, such as 203.0.113.7:47611.".into())
            } else {
                *self.agent.settings.lock().unwrap() = draft.clone();
                self.agent
                    .save_settings()
                    .map(|()| "Saved. A new rescan interval or away-from-home setting applies from the next start.".into())
                    .map_err(|e| e.to_string())
            });
        }
        if let Some(outcome) = &self.settings_outcome {
            outcome_label(ui, outcome);
        }
    }
}

/// Draws a QR code, [`module`] points per module, with its quiet zone.
fn draw_qr(ui: &mut egui::Ui, qr: &Qr, module: f32) {
    if qr.width == 0 {
        return;
    }
    let side = (qr.width + 8) as f32 * module;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
    for (i, dark) in qr.dark.iter().enumerate() {
        if *dark {
            let (x, y) = ((i % qr.width + 4) as f32, (i / qr.width + 4) as f32);
            let min = rect.min + egui::vec2(x * module, y * module);
            painter.rect_filled(egui::Rect::from_min_size(min, egui::vec2(module, module)), egui::CornerRadius::ZERO, Color32::BLACK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_invitation_fits_a_qr_code() {
        let qr = Qr::of("droidtop-pair:1?code=123456&id=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff&name=DESKTOP-PC&at=192.168.1.20:47612").unwrap();
        assert_eq!(qr.dark.len(), qr.width * qr.width);
        assert!(qr.dark.iter().any(|d| *d));
    }

    #[test]
    fn the_archive_lists_each_devices_copy_then_older_ones() {
        let root = std::env::temp_dir().join(format!("dtagent-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("steam_440");
        let save = root.join("save.dat");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&save, b"x").unwrap();
        for device in ["DESKTOP", "DESKTOP", "Retroid Pocket 5"] {
            savesync::keep_copy(std::iter::once(("<base>/save.dat".to_string(), save.clone())), &game, device).unwrap();
        }
        let titles =
            std::collections::BTreeMap::from([("steam_440".to_string(), ("steam:440".to_string(), "Team Fortress 2".to_string()))]);
        let list = archived(&root, &titles);
        // One game, named by its title and not by its folder.
        assert_eq!((list.len(), list[0].title.as_str(), list[0].key.as_deref()), (1, "Team Fortress 2", Some("steam:440")));
        let labels: Vec<&str> = list[0].copies.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(&labels[..2], &["Newest from DESKTOP", "Newest from Retroid Pocket 5"]);
        assert_eq!(labels[2], "Older, from DESKTOP");
        assert!(list[0].copies.iter().all(|a| a.files == 1));
        assert!(list[0].copies[2].when.ends_with("UTC"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stamps_read_as_dates_and_sources_as_names() {
        assert_eq!(pretty_stamp("20261008T224500Z"), "2026-10-08 22:45 UTC");
        assert_eq!(source_name("battlenet"), "Battle.net");
        assert_eq!(source_name("folder"), "Game folder");
        assert_eq!(source_name("Battle.net"), "Battle.net");
    }

    #[test]
    fn times_read_as_words() {
        assert_eq!(ago(0), "never");
        assert_eq!(ago(now_ms() - 5 * 60_000), "5 min ago");
    }
}
