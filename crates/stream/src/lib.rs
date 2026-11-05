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
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

// The agent's tunnel as windowcast's carrier (Droidtop/tracker#113); also
// built for its tests on Linux x86_64, where nothing serves it.
#[cfg(any(windows, all(test, target_os = "linux", target_arch = "x86_64")))]
pub mod carried;

/// The port windowcast's host listens on, the one a handheld assumes when the
/// answer names no address.
pub const PORT: u16 = 47100;

/// One tunnel session's side link and datagrams, as lib-sync hands them
/// over (`TunnelStream::side` and `TunnelStream::datagrams`). An empty read
/// on the side link is its end; dropping `side_out` closes it.
pub struct Carried {
    pub side_out: Sender<Vec<u8>>,
    pub side_in: Receiver<Vec<u8>>,
    pub datagrams_out: Sender<Vec<u8>>,
    pub datagrams_in: Receiver<Vec<u8>>,
}

impl Carried {
    /// Waits for the other end to say something on the side link: the
    /// handheld starting a stream's signaling. Its first bytes, or `None`
    /// when the tunnel ended first.
    pub fn first_words(&self) -> Option<Vec<u8>> {
        self.side_in.recv().ok().filter(|chunk| !chunk.is_empty())
    }
}

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

/// A viewer connected to the embedded host now, as the window shows it
/// (Droidtop/tracker#116).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watcher {
    /// The viewer's windowcast identity in hex: a paired handheld's `viewer`.
    pub viewer: String,
    /// Where it connected from.
    pub address: String,
    /// How long it has been connected, in seconds.
    pub for_secs: u64,
    /// How many of this computer's windows it watches now.
    pub windows: usize,
}

/// The embedded host, started on demand.
pub struct Streamer {
    state: Arc<Mutex<State>>,
    /// The serving host: its control (who is connected and what each
    /// watches), its windows, and the runtime it runs on, which a stream
    /// carried by the agent's tunnel uses too.
    #[cfg(windows)]
    serving: Mutex<Option<Serving>>,
}

/// What the embedded host serves with, kept for a carried stream.
#[cfg(windows)]
#[derive(Clone)]
struct Serving {
    control: Arc<windowcast_host::HostControl>,
    source: Arc<dyn windowcast_host::WindowSource>,
    runtime: tokio::runtime::Handle,
}

impl Default for Streamer {
    fn default() -> Self {
        Streamer::new()
    }
}

impl Streamer {
    pub fn new() -> Self {
        Streamer {
            state: Arc::new(Mutex::new(State::Off)),
            #[cfg(windows)]
            serving: Mutex::new(None),
        }
    }

    /// Makes sure a windowcast host serves this computer's windows, starting
    /// it the first time. [`data_dir`] holds the identity and trusted list it
    /// shares with the agent.
    pub fn ensure(&self, data_dir: &Path) -> Result<Hosting, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, State::Serving) {
            return Ok(Hosting::Here);
        }
        let hosting = match start(data_dir, Arc::clone(&self.state))? {
            Started::Here(_serving) => {
                #[cfg(windows)]
                {
                    *self.serving.lock().unwrap_or_else(|e| e.into_inner()) = Some(_serving);
                }
                *state = State::Serving;
                Hosting::Here
            }
            Started::Elsewhere => Hosting::Elsewhere,
        };
        Ok(hosting)
    }

    /// Keeps the viewer `viewer` (hex) to the windows `globs` match, app id
    /// or title (windowcast's `HostControl::limit_device`,
    /// Droidtop/tracker#116); empty takes the limit away. Sessions it starts
    /// after this use it. Nothing to do where this process does not serve.
    pub fn limit_viewer(&self, viewer: &str, globs: Vec<String>) -> Result<(), String> {
        #[cfg(windows)]
        {
            let peer = windowcast_identity::PeerId::from_hex(viewer).map_err(|e| format!("not a viewer id ({e})"))?;
            if let Some(serving) = self.serving.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                serving.control.limit_device(peer, globs);
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (viewer, globs);
            Ok(())
        }
    }

    /// The viewers connected to the embedded host now; none while it does not
    /// serve here (another program on the port serves its own).
    pub fn watching(&self) -> Vec<Watcher> {
        #[cfg(windows)]
        {
            if !matches!(*self.state.lock().unwrap_or_else(|e| e.into_inner()), State::Serving) {
                return Vec::new();
            }
            let Some(control) = self.serving.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| Arc::clone(&s.control)) else {
                return Vec::new();
            };
            let streams = control.streams();
            let mut out: Vec<Watcher> = control
                .clients()
                .into_iter()
                .map(|c| Watcher {
                    viewer: c.peer.to_hex(),
                    address: c.address.clone(),
                    for_secs: c.since.elapsed().as_secs(),
                    windows: streams.iter().filter(|s| s.peer == c.peer).count(),
                })
                .collect();
            out.sort_by_key(|w| std::cmp::Reverse(w.for_secs));
            out
        }
        #[cfg(not(windows))]
        Vec::new()
    }

    /// Serves a handheld whose stream comes through the agent's own tunnel
    /// (Droidtop/tracker#113): windowcast's signaling on the tunnel's side
    /// link, its media on the tunnel's datagrams. `first` is what
    /// [`Carried::first_words`] read. Blocks until the session ends,
    /// so it runs on the tunnel session's own thread. Only this process's
    /// host can serve it: another program on windowcast's port has its own
    /// listener and no way in to the tunnel.
    #[cfg(windows)]
    pub fn serve_carried(&self, data_dir: &Path, carried: Carried, first: Vec<u8>, address: String) -> Result<(), String> {
        if self.ensure(data_dir)? == Hosting::Elsewhere {
            return Err(format!(
                "another program listens on windowcast's port {PORT}, so a stream from away cannot come through the agent's tunnel"
            ));
        }
        let Some(serving) = self.serving.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Err("the streaming host is not serving".into());
        };
        serving.runtime.block_on(carried::serve(carried, first, address, serving.control, serving.source))
    }

    #[cfg(not(windows))]
    pub fn serve_carried(&self, _data_dir: &Path, _carried: Carried, _first: Vec<u8>, _address: String) -> Result<(), String> {
        Err("droidtop-agent streams from Windows only for now".into())
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

/// What [`start`] found: its own host, with the control the window reads, or
/// another program already on the port.
enum Started {
    #[cfg(windows)]
    Here(Serving),
    #[cfg(not(windows))]
    #[allow(dead_code)]
    Here(()),
    #[cfg_attr(not(windows), allow(dead_code))]
    Elsewhere,
}

#[cfg(windows)]
fn start(data_dir: &Path, state: Arc<Mutex<State>>) -> Result<Started, String> {
    use windowcast_agent_windows::{Options, WindowsSource};
    use windowcast_host::{HostConfig, HostControl};

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
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Ok(Started::Elsewhere),
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
    // The host's own control, kept so the window can show who is watching.
    let control = HostControl::open(&config).map_err(|e| format!("could not start the streaming host ({e})"))?;
    let source: Arc<dyn windowcast_host::WindowSource> = Arc::new(source);
    let kept = Serving { control: Arc::clone(&control), source: Arc::clone(&source), runtime: runtime.handle().clone() };
    let serving = control;
    std::thread::Builder::new()
        .name("windowcast-host".into())
        .spawn(move || {
            let served = runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)?;
                windowcast_host::serve_with(listener, serving, source).await
            });
            let why = match served {
                Ok(()) => "it ended".to_string(),
                Err(e) => e.to_string(),
            };
            eprintln!("droidtop-agent: the streaming host stopped: {why}");
            *state.lock().unwrap_or_else(|e| e.into_inner()) = State::Stopped(why);
        })
        .map_err(|e| format!("could not start the streaming host ({e})"))?;
    Ok(Started::Here(kept))
}

#[cfg(not(windows))]
fn start(_data_dir: &Path, _state: Arc<Mutex<State>>) -> Result<Started, String> {
    Err("this computer cannot stream a game yet: droidtop-agent streams from Windows only".into())
}

// On Windows the host really starts, which a unit test cannot do here.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn elsewhere_it_says_it_cannot_stream() {
        let s = Streamer::new();
        assert!(!supported());
        assert!(s.ensure(Path::new(".")).unwrap_err().contains("Windows only"));
        assert!(s.status().contains("Windows only"));
        assert!(s.watching().is_empty());
    }
}
