//! Waiting for the one device that connects to a pairing port.
//!
//! Three places wait on a listener for a device to pair or to pick this one (the handheld's pairing screen, the computer's
//! `pair` command and window, the comparison offer). They each polled a non-blocking `accept` every 100 ms and kept their own
//! flag, so a cancel or a deadline was noticed up to a tenth of a second late and the loop was written three times. This
//! blocks in `accept`, and ends it from outside the way a blocking accept can be ended: by connecting to it. A cancel and a
//! deadline both do that, and the loop looks at which one it was.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Ends a wait from another thread. Cheap to share: callers keep an `Arc<Cancel>`.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    wake: Mutex<Option<SocketAddr>>,
}

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Ends the wait now, whether it has started or not.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(at) = *self.wake.lock().unwrap_or_else(|e| e.into_inner()) {
            wake(at);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn arm(&self, at: Option<SocketAddr>) {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = at;
    }
}

/// A connection to the listener's own port from this computer: it makes a blocked `accept` return.
fn wake(listener: SocketAddr) {
    let at = SocketAddr::from(([127, 0, 0, 1], listener.port()));
    let _ = TcpStream::connect_timeout(&at, Duration::from_secs(1));
}

/// Why a wait ended without a connection.
#[derive(Debug)]
pub enum Ended {
    Cancelled,
    TimedOut,
    Io(io::Error),
}

/// A listener being waited on, until [`Cancel`] or the deadline.
pub struct Waiting<'a> {
    listener: &'a TcpListener,
    cancel: &'a Cancel,
    timed_out: Arc<AtomicBool>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    timer: Option<JoinHandle<()>>,
}

impl<'a> Waiting<'a> {
    /// Starts waiting on [`listener`]. No [`deadline`] waits until cancelled.
    pub fn new(listener: &'a TcpListener, cancel: &'a Cancel, deadline: Option<Instant>) -> io::Result<Waiting<'a>> {
        listener.set_nonblocking(false)?;
        let at = listener.local_addr()?;
        cancel.arm(Some(at));
        let timed_out = Arc::new(AtomicBool::new(false));
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let timer = deadline.map(|deadline| {
            let (timed_out, finished) = (timed_out.clone(), finished.clone());
            std::thread::spawn(move || {
                let (done, ended) = &*finished;
                let mut done = done.lock().unwrap_or_else(|e| e.into_inner());
                while !*done {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        timed_out.store(true, Ordering::SeqCst);
                        wake(at);
                        return;
                    }
                    done = ended.wait_timeout(done, left).unwrap_or_else(|e| e.into_inner()).0;
                }
            })
        });
        Ok(Waiting { listener, cancel, timed_out, finished, timer })
    }

    /// The next device to connect. A cancel or the deadline ends it, even when one is already past.
    pub fn accept(&self) -> Result<(TcpStream, SocketAddr), Ended> {
        loop {
            self.check()?;
            match self.listener.accept() {
                Ok(connection) => {
                    // The connection that woke this wait is not a device.
                    self.check()?;
                    return Ok(connection);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Ended::Io(e)),
            }
        }
    }

    fn check(&self) -> Result<(), Ended> {
        if self.cancel.is_cancelled() {
            Err(Ended::Cancelled)
        } else if self.timed_out.load(Ordering::SeqCst) {
            Err(Ended::TimedOut)
        } else {
            Ok(())
        }
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.cancel.arm(None);
        let (done, ended) = &*self.finished;
        *done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        ended.notify_all();
        if let Some(timer) = self.timer.take() {
            let _ = timer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener() -> TcpListener {
        TcpListener::bind(("127.0.0.1", 0)).unwrap()
    }

    #[test]
    fn a_connection_is_accepted() {
        let l = listener();
        let port = l.local_addr().unwrap().port();
        let cancel = Cancel::new();
        let waiting = Waiting::new(&l, &cancel, None).unwrap();
        std::thread::spawn(move || {
            let _ = TcpStream::connect(("127.0.0.1", port));
        });
        assert!(waiting.accept().is_ok());
    }

    #[test]
    fn a_cancel_from_another_thread_ends_a_blocked_wait_at_once() {
        let l = listener();
        let cancel = Arc::new(Cancel::new());
        let waiting = Waiting::new(&l, &cancel, None).unwrap();
        let from = cancel.clone();
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            from.cancel();
        });
        assert!(matches!(waiting.accept(), Err(Ended::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_deadline_ends_a_wait() {
        let l = listener();
        let cancel = Cancel::new();
        let waiting = Waiting::new(&l, &cancel, Some(Instant::now() + Duration::from_millis(150))).unwrap();
        assert!(matches!(waiting.accept(), Err(Ended::TimedOut)));
    }

    #[test]
    fn a_cancel_before_the_wait_starts_is_not_lost() {
        let l = listener();
        let cancel = Cancel::new();
        cancel.cancel();
        let waiting = Waiting::new(&l, &cancel, None).unwrap();
        assert!(matches!(waiting.accept(), Err(Ended::Cancelled)));
    }
}
