//! The process-environment scrub, and the overlay application rule it feeds.
//!
//! `compose` diffs its Meson base against the caller it is given, but that base
//! is a child of this process, and the project template context reads
//! `std::env` too. The only way all three agree is to remove the graphics and
//! loader state from this process once, before composing anything. The caller
//! passed to `compose` is then exactly the environment the runner will inherit.
//!
//! `PATH` is deliberately not scrubbed: it is the tool search path, and the
//! build system's base prepends its own entries to it.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

pub use wits_actions::Env;
use wits_actions::Overlay;
use wits_project::DevenvEntry;

/// Graphics and loader prefixes removed from the plugin's own environment.
const PREFIXES: &[&str] = &[
    "VK_", "MESA_", "LIBGL_", "EGL_", "__EGL_", "GLX_", "__GLX_", "RADV_", "R600_", "RADEON_",
    "ZINK_", "NIR_", "RUSTICL_", "OCL_ICD_", "GBM_", "AMD_", "AMDVLK_", "LIBVA_", "VDPAU_",
];

/// Exact names removed beside the prefixes.
const EXACT: &[&str] = &[
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "LD_AUDIT",
    "GALLIUM_DRIVER",
    "DRIRC_CONFIGDIR",
    "DRI_PRIME",
    "DRI_DEVICE_NAME",
    "WITS_DEVENV",
];

/// Is this name caller graphics state rather than ordinary environment?
pub fn is_scrubbed(name: &std::ffi::OsStr) -> bool {
    let name = name.as_bytes();
    PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix.as_bytes()))
        || EXACT.iter().any(|exact| name == exact.as_bytes())
}

/// Remove every scrubbed name from this process. Called once, before the first
/// `compose`, by `run` and `plan`; `install` does not compose and must not call
/// it.
pub fn scrub_process_env() {
    let names: Vec<OsString> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| is_scrubbed(name))
        .collect();
    for name in names {
        std::env::remove_var(name);
    }
}

/// The plugin's whole environment after the scrub.
pub fn caller() -> Env {
    std::env::vars_os().collect()
}

/// Apply a composed overlay to an environment.
pub fn apply_overlay(env: &mut Env, overlay: &Overlay) {
    for (name, value) in &overlay.set {
        env.insert(name.clone(), value.clone());
    }
    for name in &overlay.unset {
        env.remove(name);
    }
}

/// Apply one literal `devenv` declaration through the project registry's own
/// rule.
pub fn apply_entry(env: &mut Env, entry: &DevenvEntry) {
    entry.apply(env);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_scrub_covers_prefixes_and_exact_names_only() {
        for name in [
            "VK_DRIVER_FILES",
            "MESA_LOADER_DRIVER_OVERRIDE",
            "__EGL_VENDOR_LIBRARY_FILENAMES",
            "LIBGL_DRIVERS_PATH",
            "GBM_BACKENDS_PATH",
            "LD_LIBRARY_PATH",
            "DRIRC_CONFIGDIR",
            "WITS_DEVENV",
        ] {
            assert!(is_scrubbed(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "LD_LIBRARY_PATHX", "VK", "MESA"] {
            assert!(!is_scrubbed(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn the_overlay_application_removes_unsets_and_touches_nothing_else() {
        use std::collections::BTreeMap;
        let mut env: Env = BTreeMap::new();
        env.insert("KEEP".into(), "x".into());
        env.insert("GONE".into(), "y".into());
        let overlay = Overlay {
            set: vec![("ADD".into(), "a".into())],
            unset: vec!["GONE".into()],
        };
        apply_overlay(&mut env, &overlay);
        assert_eq!(env.get(OsStr::new("ADD")).unwrap(), "a");
        assert_eq!(env.get(OsStr::new("KEEP")).unwrap(), "x");
        assert!(!env.contains_key(OsStr::new("GONE")));
    }
}
