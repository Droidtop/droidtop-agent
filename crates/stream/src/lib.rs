//! Streaming the window of a game this computer started for a handheld,
//! with its sound and gamepads (docs/DESIGN.md section 22). This is
//! windowcast's host, embedded: the windowcast library serves, and this
//! crate only starts it, once, the first time a handheld asks to watch a
//! game, and keeps it for the life of the process. Pythia, which hosts the
//! agent in its own process, gets it through the agent.
//!
//! The host uses the identity and trusted list the agent already shares
//! with windowcast on this computer (`windowcast_identity::computer_dir`),
//! so a handheld's stream answers as the computer it paired with. PIN
//! pairing stays closed: a handheld's viewer is trusted because the agent
//! pinned it for a paired handheld that may start games here.
//!
//! Windows only for now: windowcast's Linux host needs a wlroots compositor
//! and PulseAudio, and there is no macOS host.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// The port windowcast's host listens on, the one a handheld assumes when the
/// answer names no address.
pub const PORT: u16 = 47100;

/// Whether this build can host a stream on this system.
pub const fn supported() -> bool {
    cfg!(windows)
}

/// Who serves windowcast's port on this computer once [`Streamer::ensure`]
/// returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosting {
    /// This process: the embedded host.
    Here,
    /// Another program already listens on windowcast's port, most likely
    /// windowcast's own app on the same identity and trusted list. It is used
    /// as it is.
    Elsewhere,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(windows), allow(dead_code))]
enum State {
    Off,
    Serving,
    /// The host stopped, and why; the next ask starts it again.
    Stopped(String),
}

/// The embedded host, started on demand.
pub struct Streamer {
    state: Arc<Mutex<State>>,
}

impl Default for Streamer {
    fn default() -> Self {
        Streamer::new()
    }
}

impl Streamer {
    pub fn new() -> Self {
        Streamer { state: Arc::new(Mutex::new(State::Off)) }
    }

    /// Makes sure a windowcast host serves this computer's windows, starting
    /// it the first time. [`data_dir`] holds the identity and trusted list it
    /// shares with the agent.
    pub fn ensure(&self, data_dir: &Path) -> Result<Hosting, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, State::Serving) {
            return Ok(Hosting::Here);
        }
        let hosting = start(data_dir, Arc::clone(&self.state))?;
        if hosting == Hosting::Here {
            *state = State::Serving;
        }
        Ok(hosting)
    }

    /// In words, for the window and the log: whether it serves, and why it
    /// stopped when it did.
    pub fn status(&self) -> String {
        match &*self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            State::Off if supported() => "not started: it starts when a handheld asks to watch a game".into(),
            State::Off => "this computer cannot stream yet (Windows only)".into(),
            State::Serving => format!("streaming host on port {PORT}"),
            State::Stopped(why) => format!("the streaming host stopped: {why}"),
        }
    }
}

#[cfg(windows)]
fn start(data_dir: &Path, state: Arc<Mutex<State>>) -> Result<Hosting, String> {
    use windowcast_agent_windows::{Options, WindowsSource};
    use windowcast_host::HostConfig;

    // Captures and injected input are in physical pixels; a process that is
    // not DPI-aware gets a scaled view of a scaled monitor. A process whose
    // window already set it (the app) keeps what it set.
    // SAFETY: a plain Win32 call with a constant argument.
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let listener = match std::net::TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Ok(Hosting::Elsewhere),
        Err(e) => return Err(format!("could not listen on port {PORT} ({e})")),
    };
    listener.set_nonblocking(true).map_err(|e| format!("could not listen on port {PORT} ({e})"))?;
    // Games: 60 frames a second, which the host steps down when the link is thin.
    let source = WindowsSource::new(Options { fps: 60, ..Options::default() })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("windowcast-host")
        .build()
        .map_err(|e| format!("could not start the streaming host ({e})"))?;
    let config = HostConfig { listen: format!("0.0.0.0:{PORT}"), pairing: false, data_dir: data_dir.to_path_buf() };
    std::thread::Builder::new()
        .name("windowcast-host".into())
        .spawn(move || {
            let served = runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)?;
                windowcast_host::serve(listener, config, Arc::new(source)).await
            });
            let why = match served {
                Ok(()) => "it ended".to_string(),
                Err(e) => e.to_string(),
            };
            eprintln!("droidtop-agent: the streaming host stopped: {why}");
            *state.lock().unwrap_or_else(|e| e.into_inner()) = State::Stopped(why);
        })
        .map_err(|e| format!("could not start the streaming host ({e})"))?;
    Ok(Hosting::Here)
}

#[cfg(not(windows))]
fn start(_data_dir: &Path, _state: Arc<Mutex<State>>) -> Result<Hosting, String> {
    Err("this computer cannot stream a game yet: droidtop-agent streams from Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_it_says_it_cannot_stream() {
        let s = Streamer::new();
        assert!(!supported());
        assert!(s.ensure(Path::new(".")).unwrap_err().contains("Windows only"));
        assert!(s.status().contains("Windows only"));
    }
}
