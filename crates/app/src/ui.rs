//! The window: one page per job, picked on the left. Everything slow (a
//! scan, pairing, fetching an adapter) runs on a thread of its own and the
//! page shows how it went; nothing on the drawing thread touches the network,
//! and files are read only when a page opens or the person asks.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droidtop_agent::state::{Agent, Settings};
use droidtop_agent::{autostart, pair, rendezvous, state};
use droidtop_agent_core::keys::{short, PeerId};
use droidtop_agent_core::library::now_ms;
use eframe::egui::{self, Color32, RichText};

use crate::tray::{Action, Tray};

/// How the in-process service is doing.
#[derive(Debug, Clone)]
pub enum Service {
    Starting,
    Running,
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
        addresses: Vec<String>,
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

/// One conflict loser or refused save the agent kept.
struct Archived {
    game: String,
    when: String,
    files: usize,
    path: PathBuf,
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
    archive: Vec<Archived>,
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
            Page::Saves => self.archive = archived(&self.agent.dirs.archive()),
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

/// The archive: `<game>/<UTC time>/` folders, newest first.
fn archived(root: &Path) -> Vec<Archived> {
    let mut out = Vec::new();
    for game in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for when in std::fs::read_dir(game.path()).into_iter().flatten().flatten() {
            let files = walk_count(&when.path(), 0);
            out.push(Archived {
                game: game.file_name().to_string_lossy().into_owned(),
                when: when.file_name().to_string_lossy().into_owned(),
                files,
                path: when.path(),
            });
        }
    }
    out.sort_by(|a, b| b.when.cmp(&a.when));
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
        let devices = self.agent.devices.lock().unwrap().clone();
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
        ui.strong("Away from home");
        let settings = self.agent.settings.lock().unwrap().clone();
        if !settings.rendezvous {
            ui.label(
                "Off: paired handhelds reach this computer only on this network, through a forwarded port, or through the cloud folder.",
            );
        } else {
            match &self.rendezvous {
                Some(st) => {
                    if let Some(mapped) = &st.mapped {
                        ui.label(format!("Your router shows this computer as {mapped}; handhelds find it through global discovery."));
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
            Pairing::Showing { code, addresses, qr, until, .. } => {
                ui.label("On the handheld: Settings > Computers > Pair a computer > Use a code from the computer, and type:");
                ui.add_space(6.0);
                egui::Grid::new("showing").num_columns(2).show(ui, |ui| {
                    ui.label("Address");
                    ui.label(RichText::new(addresses.join("   or   ")).size(20.0).monospace());
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
                        addresses: showing.addresses.clone(),
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

    fn library(&mut self, ui: &mut egui::Ui) {
        ui.heading("Library");
        ui.add_space(6.0);
        let scanning = self.scanning.load(std::sync::atomic::Ordering::Relaxed);
        ui.horizontal(|ui| {
            if ui.add_enabled(!scanning, egui::Button::new(if scanning { "Scanning…" } else { "Scan now" })).clicked() {
                self.scanning.store(true, std::sync::atomic::Ordering::Relaxed);
                let (agent, flag) = (self.agent.clone(), self.scanning.clone());
                std::thread::spawn(move || {
                    agent.rescan();
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                });
            }
            ui.add(egui::TextEdit::singleline(&mut self.library_filter).hint_text("Filter").desired_width(200.0));
        });
        let scan = self.agent.scan.lock().unwrap();
        let Some((at, scan)) = scan.as_ref() else {
            ui.label("No scan yet; it runs when the agent starts and every half hour.");
            return;
        };
        ui.label(RichText::new(format!("Scanned {} min ago.", at.elapsed().as_secs() / 60)).weak());
        let filter = self.library_filter.to_lowercase();
        let wanted = |text: &str| filter.is_empty() || text.to_lowercase().contains(&filter);
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Games ({})", scan.games.len())).default_open(true).show(ui, |ui| {
            egui::Grid::new("games").striped(true).num_columns(3).show(ui, |ui| {
                for f in scan.games.iter().filter(|f| wanted(&f.game.title)) {
                    ui.label(&f.game.title);
                    ui.label(RichText::new(f.game.install.launcher.clone().unwrap_or_default()).weak());
                    ui.label(RichText::new(f.game.install.path.clone().unwrap_or_default()).weak().small());
                    ui.end_row();
                }
            });
        });
        egui::CollapsingHeader::new(format!("Apps ({})", scan.apps.len())).show(ui, |ui| {
            egui::Grid::new("apps").striped(true).num_columns(3).show(ui, |ui| {
                for a in scan.apps.iter().filter(|a| wanted(&a.name)) {
                    ui.label(&a.name);
                    ui.label(RichText::new(a.source_label()).weak());
                    ui.label(RichText::new(a.version.clone().unwrap_or_default()).weak());
                    ui.end_row();
                }
            });
        });
        egui::CollapsingHeader::new(format!("Programs droidtop can sync with ({})", scan.programs.len())).show(ui, |ui| {
            for p in &scan.programs {
                ui.label(format!("{}: {}", p.name, p.note));
            }
        });
        if !scan.problems.is_empty() {
            ui.add_space(8.0);
            ui.strong("Could not read");
            for p in &scan.problems {
                ui.colored_label(ui.visuals().warn_fg_color, p);
            }
        }
    }

    fn saves(&mut self, ui: &mut egui::Ui) {
        ui.heading("Saves");
        ui.add_space(6.0);
        ui.label(
            "When the handheld and this computer both changed a game's saves, you choose on the handheld which to keep. If this computer's copy loses, or a save the handheld left in the cloud folder no longer fits, it is kept here.",
        );
        ui.add_space(8.0);
        if self.archive.is_empty() {
            ui.label("Nothing is kept.");
        }
        egui::Grid::new("archive").striped(true).num_columns(4).show(ui, |ui| {
            for a in &self.archive {
                ui.label(&a.game);
                ui.label(&a.when);
                ui.label(format!("{} files", a.files));
                if ui.button("Open folder").clicked() {
                    open_folder(&a.path);
                }
                ui.end_row();
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
    fn the_archive_lists_each_kept_copy_newest_first() {
        let root = std::env::temp_dir().join(format!("dtagent-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("steam_440/20261001T100000Z/a")).unwrap();
        std::fs::create_dir_all(root.join("steam_440/20261002T100000Z")).unwrap();
        std::fs::write(root.join("steam_440/20261001T100000Z/a/save.dat"), b"x").unwrap();
        std::fs::write(root.join("steam_440/20261001T100000Z/b.dat"), b"x").unwrap();
        let list = archived(&root);
        assert_eq!(
            list.iter().map(|a| (a.when.as_str(), a.files)).collect::<Vec<_>>(),
            vec![("20261002T100000Z", 0), ("20261001T100000Z", 2)]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn times_read_as_words() {
        assert_eq!(ago(0), "never");
        assert_eq!(ago(now_ms() - 5 * 60_000), "5 min ago");
    }
}
