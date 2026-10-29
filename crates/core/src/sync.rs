//! Locking that survives a panic elsewhere.
//!
//! A `Mutex` is poisoned when a thread panics while holding it, and every later `lock().unwrap()` then panics too: one failed
//! maintenance thread would take the window, the command line and the Android bridge down with it, and a panic across the JNI
//! boundary aborts the handheld's app. What these mutexes guard is plain state that is rewritten whole (settings, the paired
//! devices, outcomes shown to the person), so the data a panicking thread left is still the data to carry on with.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

pub trait LockExt<T> {
    /// The lock, taken even if a thread panicked while holding it.
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Something happened that waiting threads want to know about, without polling: a setting was saved, the agent was told to
/// stop. A waiter remembers the [`generation`](Pulse::current) it has seen and sleeps until it changes or its time is up.
#[derive(Default)]
pub struct Pulse {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl Pulse {
    pub fn new() -> Pulse {
        Pulse::default()
    }

    /// The generation now. Take it before the work that decides what to wait for, then [`wait`](Pulse::wait) with it: a
    /// [`notify`](Pulse::notify) in between is not lost.
    pub fn current(&self) -> u64 {
        *self.generation.locked()
    }

    pub fn notify(&self) {
        *self.generation.locked() += 1;
        self.changed.notify_all();
    }

    /// Sleeps until the generation is not [`seen`] or [`timeout`] passes; returns the generation then.
    pub fn wait(&self, seen: u64, timeout: Duration) -> u64 {
        let guard = self.generation.locked();
        let (guard, _) =
            self.changed.wait_timeout_while(guard, timeout, |generation| *generation == seen).unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_lock_poisoned_by_a_panic_is_still_taken() {
        let value = Arc::new(Mutex::new(1));
        let held = value.clone();
        let _ = std::thread::spawn(move || {
            let mut guard = held.lock().unwrap();
            *guard = 2;
            panic!("a thread died holding the lock");
        })
        .join();
        assert!(value.lock().is_err(), "the lock should be poisoned");
        assert_eq!(*value.locked(), 2);
        *value.locked() = 3;
        assert_eq!(*value.locked(), 3);
    }

    #[test]
    fn a_pulse_wakes_a_waiter_and_a_notify_before_the_wait_is_not_lost() {
        let pulse = Arc::new(Pulse::new());
        let seen = pulse.current();
        let from = pulse.clone();
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            from.notify();
        });
        assert_ne!(pulse.wait(seen, Duration::from_secs(10)), seen);
        assert!(started.elapsed() < Duration::from_secs(5));
        // Notified after `seen` was taken and before the wait: the wait returns at once.
        let seen = pulse.current();
        pulse.notify();
        let started = std::time::Instant::now();
        assert_ne!(pulse.wait(seen, Duration::from_secs(10)), seen);
        assert!(started.elapsed() < Duration::from_secs(1));
        // Nothing happened: the wait lasts its time.
        let seen = pulse.current();
        assert_eq!(pulse.wait(seen, Duration::from_millis(30)), seen);
    }
}
