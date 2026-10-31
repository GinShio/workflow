//! Test-only process-global environment guards.
//!
//! The state and run tests both point `XDG_STATE_HOME`/`XDG_RUNTIME_DIR` at a
//! temporary directory. Rust runs tests in parallel, so they share one lock and
//! restore what they changed.

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

pub fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Set an environment variable and restore the previous value on drop.
pub struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
