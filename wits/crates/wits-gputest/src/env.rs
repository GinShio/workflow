//! The process-environment scrub.
//!
//! `compose` diffs Meson's developer environment against the caller it is
//! given, but Meson runs as a child of this process, and the project templates
//! read `std::env` too. The only way all three agree is to remove the graphics
//! and loader state from this process once, before the registry loads. What
//! remains is the caller every case composes against and the runner inherits,
//! which is also why a stale `MESA_*` knob in a shell cannot leak into a test.
//!
//! `PATH` is deliberately not scrubbed: it is the tool search path, and Meson
//! prepends its own entries to it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use wits_actions::Env;

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
pub fn is_scrubbed(name: &OsStr) -> bool {
    let name = name.as_bytes();
    PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix.as_bytes()))
        || EXACT.iter().any(|exact| name == exact.as_bytes())
}

/// Remove every scrubbed name from this process and return the environment
/// that is left. `run` calls it once, before the registry loads and before any
/// thread starts.
pub fn scrub() -> Env {
    let names: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| is_scrubbed(name))
        .collect();
    for name in names {
        std::env::remove_var(name);
    }
    std::env::vars_os().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
