//! Locking that survives a panic elsewhere.
//!
//! A `Mutex` is poisoned when a thread panics while holding it, and every later `lock().unwrap()` then panics too: one failed
//! maintenance thread would take the window, the command line and the Android bridge down with it, and a panic across the JNI
//! boundary aborts the handheld's app. What these mutexes guard is plain state that is rewritten whole (settings, the paired
//! devices, outcomes shown to the person), so the data a panicking thread left is still the data to carry on with.

use std::sync::{Mutex, MutexGuard};

pub trait LockExt<T> {
    /// The lock, taken even if a thread panicked while holding it.
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
}
