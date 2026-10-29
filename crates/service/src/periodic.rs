//! The agent's recurring work, and how it stops.
//!
//! The library scan, the cloud share, Syncthing and the rendezvous each ran as a bare `thread::spawn(loop { work; sleep(N) })`.
//! A changed setting (how often to scan, rendezvous on or off) was noticed only after the current sleep, nothing could stop any
//! of them, and the loop was written four times with its own error reporting. [`spawn`] is the one loop: the interval is asked
//! again every round and whenever the settings are saved, and [`Stop`] ends every task (and the listener) promptly.

use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droidtop_agent_core::sync::{LockExt, Pulse};

use crate::Agent;

/// Ends the agent's recurring work and its listener. Stopping is for good.
pub struct Stop {
    flag: Arc<AtomicBool>,
    /// The pulse recurring tasks sleep on: the store's, so a saved setting wakes them too.
    pulse: Arc<Pulse>,
    listening: Mutex<Option<SocketAddr>>,
}

impl Stop {
    pub fn new(pulse: Arc<Pulse>) -> Stop {
        Stop { flag: Arc::new(AtomicBool::new(false)), pulse, listening: Mutex::new(None) }
    }

    /// The flag for code that polls one (the discovery answerer, the WireGuard loop).
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }

    pub fn is_stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Ends every task and the listener registered with [`listening_on`](Stop::listening_on).
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.pulse.notify();
        if let Some(at) = *self.listening.locked() {
            // A blocked accept returns for a connection to its own port.
            let _ = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], at.port())), Duration::from_secs(1));
        }
    }

    /// Tells [`stop`](Stop::stop) which listener to wake.
    pub fn listening_on(&self, at: SocketAddr) {
        *self.listening.locked() = Some(at);
    }

    /// Sleeps [`time`], or less if stopped; returns whether it was stopped.
    pub fn sleep(&self, time: Duration) -> bool {
        let until = Instant::now() + time;
        let mut seen = self.pulse.current();
        while !self.is_stopped() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            seen = self.pulse.wait(seen, left);
        }
        true
    }
}

/// Runs [`work`] on its own thread: first after [`first`], then again [`every`] after each run began, until the agent is
/// stopped. [`every`] is asked again each round and whenever the settings are saved, so a changed interval applies at once.
pub fn spawn(
    name: &str,
    agent: &Arc<Agent>,
    first: Duration,
    every: impl Fn(&Agent) -> Duration + Send + 'static,
    mut work: impl FnMut(&Agent) + Send + 'static,
) {
    let agent = agent.clone();
    let spawned = std::thread::Builder::new().name(format!("droidtop-{name}")).spawn(move || {
        if agent.stop.sleep(first) {
            return;
        }
        while !agent.stop.is_stopped() {
            let began = Instant::now();
            work(&agent);
            // Taken after the work, which may itself save settings.
            let mut seen = agent.store.changes.current();
            loop {
                if agent.stop.is_stopped() {
                    return;
                }
                let left = every(&agent).saturating_sub(began.elapsed());
                if left.is_zero() {
                    break;
                }
                seen = agent.store.changes.wait(seen, left);
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("droidtop-agent could not start its {name} task: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droidtop_agent_store::Dirs;
    use std::sync::atomic::AtomicUsize;

    fn agent(name: &str) -> Arc<Agent> {
        let root = std::env::temp_dir().join(format!("dta-periodic-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs { config: root.join("agent"), data: root.join("data"), computer: root.join("windowcast/app/host") };
        for p in [&dirs.config, &dirs.data, &dirs.computer] {
            std::fs::create_dir_all(p).unwrap();
        }
        Arc::new(Agent::open_in(dirs).unwrap())
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn work_runs_at_its_interval_until_stopped() {
        let agent = agent("interval");
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        spawn(
            "test",
            &agent,
            Duration::ZERO,
            |_| Duration::from_millis(20),
            move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
            },
        );
        until("three runs", || runs.load(Ordering::SeqCst) >= 3);
        agent.stop.stop();
        std::thread::sleep(Duration::from_millis(100));
        let after = runs.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(runs.load(Ordering::SeqCst), after, "it kept running after the stop");
    }

    #[test]
    fn a_saved_setting_shortens_a_long_wait_at_once() {
        let agent = agent("setting");
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let short = Arc::new(AtomicBool::new(false));
        let asked = short.clone();
        spawn(
            "test",
            &agent,
            Duration::ZERO,
            move |_| if asked.load(Ordering::SeqCst) { Duration::from_millis(10) } else { Duration::from_secs(3600) },
            move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
            },
        );
        until("the first run", || runs.load(Ordering::SeqCst) == 1);
        short.store(true, Ordering::SeqCst);
        agent.store.save_settings().unwrap();
        until("the second run after the setting changed", || runs.load(Ordering::SeqCst) >= 2);
        agent.stop.stop();
    }

    #[test]
    fn stopping_wakes_a_task_in_a_long_wait_and_a_sleep() {
        let agent = agent("stop");
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let a = agent.clone();
        std::thread::spawn(move || {
            let stopped = a.stop.sleep(Duration::from_secs(3600));
            done.store(stopped, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(50));
        agent.stop.stop();
        until("the sleeper to wake", || finished.load(Ordering::SeqCst));
    }
}
