//! The tray icon and its menu: Open, Pair a handheld, Quit. On Windows and
//! macOS it is `tray-icon`; on Linux a StatusNotifierItem over D-Bus
//! (`ksni`), which KDE, most other desktops and GNOME with the AppIndicator
//! extension show. Where there is no tray, closing the window quits, so the
//! agent is never left running with no way back to it.

use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;

/// What the person picked in the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Pair,
    Quit,
}

pub struct Tray {
    pub actions: Receiver<Action>,
    /// Whether a tray icon is showing.
    pub shown: bool,
    #[allow(dead_code)]
    handle: Option<imp::Handle>,
}

impl Tray {
    /// Puts the icon in the tray. Called once the window's event loop runs
    /// (macOS needs that).
    pub fn start(ctx: &egui::Context) -> Tray {
        let (tx, actions) = channel();
        let notify = Notify { ctx: ctx.clone(), tx };
        match imp::start(notify) {
            Ok(handle) => Tray { actions, shown: true, handle: Some(handle) },
            Err(e) => {
                eprintln!("No tray icon ({e}); closing the window quits droidtop-agent.");
                Tray { actions, shown: false, handle: None }
            }
        }
    }
}

/// Sends an action to the window and wakes it, shown or hidden.
#[derive(Clone)]
struct Notify {
    ctx: egui::Context,
    tx: Sender<Action>,
}

impl Notify {
    fn send(&self, action: Action) {
        if action == Action::Quit {
            std::process::exit(0);
        }
        let _ = self.tx.send(action);
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.ctx.request_repaint();
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use super::{Action, Notify};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub type Handle = TrayIcon;

    pub fn start(notify: Notify) -> Result<Handle, String> {
        let open = MenuItem::new("Open droidtop-agent", true, None);
        let pair = MenuItem::new("Pair a handheld…", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &pair, &PredefinedMenuItem::separator(), &quit]).map_err(|e| e.to_string())?;
        let (rgba, size) = crate::icon::rgba(32);
        let icon = tray_icon::Icon::from_rgba(rgba, size, size).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("droidtop-agent")
            .with_icon(icon)
            .build()
            .map_err(|e| e.to_string())?;
        let ids = (open.id().clone(), pair.id().clone(), quit.id().clone());
        let on_menu = notify.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == ids.0 {
                Action::Open
            } else if event.id == ids.1 {
                Action::Pair
            } else if event.id == ids.2 {
                Action::Quit
            } else {
                return;
            };
            on_menu.send(action);
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                notify.send(Action::Open);
            }
        }));
        Ok(tray)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::{Action, Notify};
    use ksni::blocking::TrayMethods;

    pub type Handle = ksni::blocking::Handle<AgentTray>;

    pub struct AgentTray {
        notify: Notify,
    }

    impl ksni::Tray for AgentTray {
        fn id(&self) -> String {
            "droidtop-agent".into()
        }
        fn title(&self) -> String {
            "droidtop-agent".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // ksni wants ARGB32 in network byte order.
            let (rgba, size) = crate::icon::rgba(32);
            let data = rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            vec![ksni::Icon { width: size as i32, height: size as i32, data }]
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            self.notify.send(Action::Open);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            let item = |label: &str, action: Action| -> ksni::MenuItem<Self> {
                StandardItem { label: label.into(), activate: Box::new(move |t: &mut Self| t.notify.send(action)), ..Default::default() }
                    .into()
            };
            vec![
                item("Open droidtop-agent", Action::Open),
                item("Pair a handheld…", Action::Pair),
                ksni::MenuItem::Separator,
                item("Quit", Action::Quit),
            ]
        }
    }

    pub fn start(notify: Notify) -> Result<Handle, String> {
        AgentTray { notify }.spawn().map_err(|e| e.to_string())
    }
}
