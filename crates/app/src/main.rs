//! droidtop-agent-app: the agent with a window and a tray icon, so nobody
//! needs the command line (docs/DESIGN.md section 15). It runs the same agent
//! as `droidtop-agent run`, in this process, and shows it: pairing, status,
//! the library it found, saves, plugin data and settings. Closing the window
//! keeps it in the tray; Quit in the tray menu stops it.
//!
//! `--hidden` starts it in the tray only (how autostart starts it).

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use droidtop_agent_computer::state::Agent;

mod icon;
mod tray;
mod ui;

fn main() {
    let hidden = std::env::args().skip(1).any(|a| a == "--hidden");
    let agent = match Agent::open() {
        Ok(agent) => Arc::new(agent),
        Err(e) => {
            eprintln!("droidtop-agent could not start: {e}");
            std::process::exit(1);
        }
    };

    // The service: the same as `droidtop-agent run`. If it cannot listen
    // because another copy of the agent is already running, the window
    // attaches to that one instead of competing with it.
    let service: Arc<Mutex<ui::Service>> = Arc::new(Mutex::new(ui::Service::Starting));
    {
        let (agent, service) = (agent.clone(), service.clone());
        std::thread::spawn(move || {
            *service.lock().unwrap() = ui::Service::Running;
            let result = droidtop_agent_computer::serve::run(agent.clone());
            let stopped: String = match result {
                Ok(()) => "The agent stopped.".into(),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    // The running agent serves; this window shows its state from
                    // the files they share. It looks at the computer once itself,
                    // so the Library page has something to show.
                    *service.lock().unwrap() = ui::Service::Attached;
                    agent.rescan();
                    return;
                }
                Err(e) => format!("The agent could not listen: {e}"),
            };
            *service.lock().unwrap() = ui::Service::Stopped(stopped);
        });
    }

    let (rgba, size) = icon::rgba(64);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("droidtop-agent")
            .with_app_id("dev.droidtop.agent")
            .with_inner_size([760.0, 540.0])
            .with_min_inner_size([520.0, 380.0])
            .with_visible(!hidden)
            .with_icon(Arc::new(eframe::egui::IconData { rgba, width: size, height: size })),
        ..Default::default()
    };
    let result = eframe::run_native(
        "droidtop-agent",
        options,
        Box::new(move |cc| {
            let tray = tray::Tray::start(&cc.egui_ctx);
            Ok(Box::new(ui::App::new(agent, service, tray)))
        }),
    );
    if let Err(e) = result {
        eprintln!("droidtop-agent could not open its window: {e}");
        std::process::exit(1);
    }
}
