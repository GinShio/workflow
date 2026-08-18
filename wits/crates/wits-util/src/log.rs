//! Two process-wide switches and a logger that respects them.
//!
//! Verbose and dry-run are flags every subcommand can be invoked with, but
//! threading them through every call site is noise. They are genuinely global
//! to a run, so they live in two atomics set once at startup.
//!
//! The split of streams is deliberate: ordinary log lines go to stderr, while
//! the dry-run preview of a command goes to stdout. That way a script can do
//! `wits ... -n | sh` (or just capture the plan) without log chatter polluting
//! the output it cares about.

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);
static DRY_RUN: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

#[inline]
pub fn is_dry_run() -> bool {
    DRY_RUN.load(Ordering::Relaxed)
}

/// Wire up the flags and the logger. Safe to call repeatedly (tests do); the
/// logger only registers once, later calls just re-set the flags.
pub fn init(verbose: bool, dry_run: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
    DRY_RUN.store(dry_run, Ordering::Relaxed);

    let _ = log::set_logger(&WF_LOGGER);
    log::set_max_level(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
}

struct WfLogger;
static WF_LOGGER: WfLogger = WfLogger;

impl log::Log for WfLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if metadata.level() == log::Level::Debug {
            VERBOSE.load(Ordering::Relaxed)
        } else {
            metadata.level() <= log::Level::Info
        }
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // The module path is more noise than signal here; keep just the final
        // segment as a short scope tag.
        let scope = record
            .target()
            .rsplit("::")
            .next()
            .filter(|s| *s != "wits")
            .unwrap_or("");
        let level = record.level().as_str().to_uppercase();
        if scope.is_empty() {
            eprintln!("[{level}] {}", record.args());
        } else {
            eprintln!("[{level}] ({scope}) {}", record.args());
        }
    }

    fn flush(&self) {}
}

/// Emit the "would have run" line for a command. No-op unless dry-run is on,
/// so callers don't have to guard it.
pub fn dry_run(msg: &str) {
    if is_dry_run() {
        println!("[DRY-RUN] {msg}");
    }
}

/// The verbose/dry-run flags are process-global, so any test that sets them and
/// then observes the effect must run alone — otherwise a sibling test flipping
/// the same flag mid-body makes both flaky. Such tests hold this guard for their
/// duration. Poisoning is ignored: a panicking test still leaves the flags in a
/// known state because every guarded test resets them on the way out.
#[cfg(test)]
pub(crate) fn test_flag_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_sets_flags() {
        let _guard = test_flag_guard();
        init(true, true);
        assert!(is_verbose() && is_dry_run());
        init(false, false);
        assert!(!is_verbose() && !is_dry_run());
    }

    /// Info is the commands' normal feedback channel (`pushed X`, `created MR`,
    /// build steps), so it must be visible without `-v`; only Debug is gated on
    /// verbose. Guards the regression where `<= Warn` silently swallowed Info.
    #[test]
    fn info_is_visible_by_default_and_debug_needs_verbose() {
        use log::{Level, Log, Metadata};
        let _guard = test_flag_guard();
        let meta = |level: Level| Metadata::builder().level(level).build();

        init(false, false);
        assert!(WF_LOGGER.enabled(&meta(Level::Error)));
        assert!(WF_LOGGER.enabled(&meta(Level::Warn)));
        assert!(
            WF_LOGGER.enabled(&meta(Level::Info)),
            "info hidden by default"
        );
        assert!(
            !WF_LOGGER.enabled(&meta(Level::Debug)),
            "debug leaked without -v"
        );

        init(true, false);
        assert!(WF_LOGGER.enabled(&meta(Level::Info)));
        assert!(
            WF_LOGGER.enabled(&meta(Level::Debug)),
            "debug missing under -v"
        );

        init(false, false);
    }
}
