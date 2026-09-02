//! Host facts — the one place wits probes the machine.
//!
//! [`facts`] builds a single [`Value`] tree of what we can learn about the host
//! (os, cpu, memory, gpu, distro, power, desktop, virt, …). The project layer
//! exposes that tree as the `system.*` template namespace, making this module
//! the internal source of truth for host-dependent project resolution.
//!
//! A node is either a scalar (a leaf) or a map (a subtree) — never both — so a
//! dotted path like `cpu.count` or `os.kernel.major` addresses exactly one
//! value, and an intermediate path (`cpu`) names a subtree.
//!
//! Detection is best-effort and Linux-first (the `/proc`, `/sys`, `/etc/os-release`
//! reads it leans on); on another OS the facts it can't determine fall back to a
//! sensible default (`unknown`, `0`, `none`, `false`) rather than failing. The
//! result is cached for the process, since nothing here changes during a run.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use minijinja::Value;

/// One subtree, from `(name, value)` pairs. MiniJinja converts a `BTreeMap`
/// directly, but every node here is written as a pair list, so the conversion
/// is named once instead of wrapping each of them.
fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::from(BTreeMap::from(entries))
}

/// The host facts tree, built once and reused for the rest of the process.
pub fn facts() -> Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(build).clone()
}

fn build() -> Value {
    map([
        ("os", os_facts()),
        ("cpu", cpu_facts()),
        ("mem", mem_facts()),
        ("gpu", gpu_facts()),
        ("distro", distro_facts()),
        ("power", power_facts()),
        ("hostname", Value::from(hostname())),
        ("desktop", Value::from(desktop())),
        ("display", Value::from(display_server())),
        ("virt", Value::from(virt())),
    ])
}

// -- os / kernel --------------------------------------------------------------

fn os_facts() -> Value {
    let release = kernel_release();
    let (major, minor, patch) = parse_kernel(&release);
    map([
        ("name", Value::from(std::env::consts::OS)),
        (
            "kernel",
            map([
                ("release", Value::from(release)),
                ("major", Value::from(major)),
                ("minor", Value::from(minor)),
                ("patch", Value::from(patch)),
            ]),
        ),
    ])
}

fn kernel_release() -> String {
    // Linux exposes it without a spawn; elsewhere ask `uname -r`.
    if let Ok(rel) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        let rel = rel.trim();
        if !rel.is_empty() {
            return rel.to_owned();
        }
    }
    uname("-r")
}

/// `6.6.10-arch1-1` → `(6, 6, 10)`; the dotted numeric prefix, trailing suffix
/// dropped. Missing parts are 0.
fn parse_kernel(release: &str) -> (i64, i64, i64) {
    let core = release.split('-').next().unwrap_or(release);
    let mut parts = core.split('.');
    let mut next = || parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (next(), next(), next())
}

// -- cpu ----------------------------------------------------------------------

fn cpu_facts() -> Value {
    map([
        ("count", Value::from(cpu_count())),
        ("vendor", Value::from(cpu_vendor())),
        ("arch", Value::from(arch())),
    ])
}

/// Logical CPU count, falling back to 1 when the platform won't say.
fn cpu_count() -> i64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1)
}

/// Normalised architecture, matching the shell tooling's spelling (`arm64`, not
/// `aarch64`), so a fact reads the same however it was produced.
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a if a.starts_with("arm") => "arm",
        other => other, // x86_64, x86, riscv64, … pass through
    }
}

/// CPU vendor from `/proc/cpuinfo`: the x86 vendor strings directly, else the
/// ARM `CPU implementer` id mapped to a name. `unknown` off Linux or when
/// nothing matches.
fn cpu_vendor() -> String {
    let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") else {
        return "unknown".into();
    };
    for (needle, vendor) in [
        ("GenuineIntel", "intel"),
        ("AuthenticAMD", "amd"),
        ("SiFive", "sifive"),
        ("T-Head", "thead"),
        ("StarFive", "starfive"),
    ] {
        if info.contains(needle) {
            return vendor.into();
        }
    }
    if let Some(line) = info
        .lines()
        .find(|l| l.to_lowercase().starts_with("cpu implementer"))
    {
        let implementer = line.split(':').nth(1).unwrap_or("").trim().to_lowercase();
        let vendor = match implementer.as_str() {
            "0x41" => "arm",
            "0x42" => "broadcom",
            "0x43" => "cavium",
            "0x48" => "hisilicon",
            "0x4e" => "nvidia",
            "0x50" => "ampere",
            "0x51" => "qualcomm",
            "0x53" => "samsung",
            "0x61" => "apple",
            _ => "unknown",
        };
        return vendor.into();
    }
    if info.contains("BCM") {
        return "broadcom".into();
    }
    "unknown".into()
}

// -- memory -------------------------------------------------------------------

fn mem_facts() -> Value {
    let mb = total_memory_mb().unwrap_or(0);
    let gb = if mb > 0 { (mb / 1024).max(1) } else { 0 };
    map([("mb", Value::from(mb)), ("gb", Value::from(gb))])
}

/// Total physical memory in MiB, or `None` where the platform has no cheap query.
fn total_memory_mb() -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: i64 = line
            .trim_start_matches("MemTotal:")
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some((kb / 1024).max(1))
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
    {
        let key = if cfg!(target_os = "macos") {
            "hw.memsize"
        } else {
            "hw.physmem"
        };
        let out = crate::process::Command::new("sysctl")
            .args(["-n", key])
            .force_run()
            .exec()
            .ok()?;
        let bytes: i64 = out.stdout_trimmed().parse().ok()?;
        Some((bytes / (1024 * 1024)).max(1))
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd"
    )))]
    {
        None
    }
}

// -- gpu ----------------------------------------------------------------------

fn gpu_facts() -> Value {
    let list = gpu_vendors();
    map([
        ("count", Value::from(list.len() as i64)),
        ("list", Value::from(list)),
    ])
}

/// The distinct GPU vendors, from each real DRM card's bound driver. Connector
/// nodes (`card0-DP-1`) and driverless cards are skipped; empty off Linux.
fn gpu_vendors() -> Vec<String> {
    let mut vendors: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return vendors;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A card is `card<N>`; `card<N>-<connector>` are outputs, not devices.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let Ok(target) = std::fs::read_link(entry.path().join("device/driver")) else {
            continue;
        };
        let driver = target
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let vendor = match driver.as_str() {
            "amdgpu" | "radeon" => "amd",
            "i915" | "xe" | "iris" => "intel",
            "nvidia" | "nvidia-drm" => "nvidia",
            "vc4" | "v3d" => "videocore",
            "panfrost" | "mali" => "mali",
            "virtio_gpu" => "virtio",
            d if d.starts_with("tegra") || d.starts_with("nv") => "nvidia",
            _ => continue,
        };
        if !vendors.iter().any(|v| v == vendor) {
            vendors.push(vendor.to_owned());
        }
    }
    vendors
}

// -- distro -------------------------------------------------------------------

fn distro_facts() -> Value {
    let mut id = String::new();
    let mut name = String::new();
    let mut version = String::new();
    if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "ID" => id = normalize_distro(&value),
                "PRETTY_NAME" => name = value,
                "VERSION_ID" => version = value,
                _ => {}
            }
        }
    }
    map([
        ("id", Value::from(fallback(id, "unknown"))),
        ("name", Value::from(fallback(name, "unknown"))),
        ("version", Value::from(version)),
    ])
}

/// Fold the opensuse variants (leap/tumbleweed) back to plain `opensuse`.
fn normalize_distro(id: &str) -> String {
    if id.starts_with("opensuse") {
        "opensuse".into()
    } else {
        id.into()
    }
}

// -- power --------------------------------------------------------------------

fn power_facts() -> Value {
    let laptop = is_laptop();
    map([
        ("laptop", Value::from(laptop)),
        ("ac", Value::from(is_on_ac(laptop))),
    ])
}

/// A battery in `/sys/class/power_supply`, or a portable DMI chassis type,
/// means a laptop. Desktops/servers and non-Linux report `false`.
fn is_laptop() -> bool {
    if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            if let Ok(ty) = std::fs::read_to_string(entry.path().join("type")) {
                if ty.trim().eq_ignore_ascii_case("battery") {
                    return true;
                }
            }
        }
    }
    // Chassis types: 8 Portable, 9 Laptop, 10 Notebook, 14 Sub-Notebook,
    // 30 Tablet, 31 Convertible, 32 Detachable.
    if let Ok(chassis) = std::fs::read_to_string("/sys/class/dmi/id/chassis_type") {
        if matches!(chassis.trim(), "8" | "9" | "10" | "14" | "30" | "31" | "32") {
            return true;
        }
    }
    false
}

/// A non-laptop is treated as always on mains; a laptop is on AC when some
/// mains/USB supply reports `online`.
fn is_on_ac(laptop: bool) -> bool {
    if !laptop {
        return true;
    }
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return true;
    };
    for entry in entries.flatten() {
        let ty = std::fs::read_to_string(entry.path().join("type")).unwrap_or_default();
        let ty = ty.trim();
        if ty.eq_ignore_ascii_case("mains") || ty.eq_ignore_ascii_case("usb") {
            if let Ok(online) = std::fs::read_to_string(entry.path().join("online")) {
                if online.trim() == "1" {
                    return true;
                }
            }
        }
    }
    false
}

// -- desktop / display --------------------------------------------------------

/// The desktop environment from `XDG_CURRENT_DESKTOP` / `DESKTOP_SESSION`,
/// normalised to a short name; `headless` when neither is set.
fn desktop() -> String {
    let de = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("DESKTOP_SESSION").ok())
        .unwrap_or_default()
        .to_lowercase();
    if de.is_empty() {
        return "headless".into();
    }
    for (needle, norm) in [
        ("gnome", "gnome"),
        ("plasma", "kde"),
        ("kde", "kde"),
        ("xfce", "xfce"),
        ("mate", "mate"),
        ("cinnamon", "cinnamon"),
        ("lxqt", "lxqt"),
        ("sway", "sway"),
        ("hyprland", "hyprland"),
        ("i3", "i3"),
        ("pantheon", "pantheon"),
        ("budgie", "budgie"),
    ] {
        if de.contains(needle) {
            return norm.into();
        }
    }
    de
}

/// `wayland`, `x11`, or `headless`, from the session's display env vars.
fn display_server() -> String {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "wayland".into()
    } else if std::env::var_os("DISPLAY").is_some() {
        "x11".into()
    } else {
        "headless".into()
    }
}

// -- virt / hostname ----------------------------------------------------------

/// The virtualization/container the host runs under (`kvm`, `qemu`, `docker`,
/// …), or `none` on bare metal. Prefers the authoritative `systemd-detect-virt`,
/// falling back to container marker files and the DMI product name.
fn virt() -> String {
    if let Ok(out) = crate::process::Command::new("systemd-detect-virt")
        .force_run()
        .exec()
    {
        // Prints the type on success and `none` (with a non-zero exit) otherwise.
        let v = out.stdout_trimmed();
        if !v.is_empty() {
            return v.to_owned();
        }
    }
    if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
        return "container".into();
    }
    if let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name") {
        let product = product.trim().to_lowercase();
        for (needle, name) in [
            ("qemu", "qemu"),
            ("virtualbox", "oracle"),
            ("vmware", "vmware"),
            ("kvm", "kvm"),
        ] {
            if product.contains(needle) {
                return name.into();
            }
        }
    }
    "none".into()
}

fn hostname() -> String {
    if let Ok(name) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_lowercase();
        }
    }
    uname("-n").to_lowercase()
}

// -- helpers ------------------------------------------------------------------

/// One `uname <flag>` read, empty when it can't be run (non-Unix / no `uname`).
fn uname(flag: &str) -> String {
    crate::process::Command::new("uname")
        .args([flag])
        .force_run()
        .exec()
        .ok()
        .map(|o| o.stdout_trimmed().to_owned())
        .unwrap_or_default()
}

fn fallback(value: String, default: &str) -> String {
    if value.is_empty() {
        default.to_owned()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use minijinja::value::ValueKind;

    /// A dotted lookup over the facts tree using nothing but `Value`'s own walk.
    /// The project resolver answers the same paths, but a floor module must not
    /// reach up into a subsystem to test itself.
    fn get(root: &Value, path: &str) -> Option<Value> {
        path.split('.').try_fold(root.clone(), |current, part| {
            current.get_attr(part).ok().filter(|v| !v.is_undefined())
        })
    }

    #[test]
    fn parses_kernel_release() {
        assert_eq!(parse_kernel("6.6.10-arch1-1"), (6, 6, 10));
        assert_eq!(parse_kernel("5.15.0"), (5, 15, 0));
        assert_eq!(parse_kernel("6.1"), (6, 1, 0));
        assert_eq!(parse_kernel("weird"), (0, 0, 0));
    }

    #[test]
    fn arch_is_normalised() {
        // Whatever the host is, the value is non-empty and never the raw aarch64.
        assert!(!arch().is_empty());
        assert_ne!(arch(), "aarch64");
    }

    #[test]
    fn normalizes_opensuse_variants() {
        assert_eq!(normalize_distro("opensuse-tumbleweed"), "opensuse");
        assert_eq!(normalize_distro("opensuse-leap"), "opensuse");
        assert_eq!(normalize_distro("arch"), "arch");
    }

    #[test]
    fn facts_tree_has_the_core_shape() {
        let facts = facts();
        let kind = |path: &str| get(&facts, path).map(|v| v.kind());

        // Intermediate nodes are subtrees; leaves are scalars.
        let cpus = get(&facts, "cpu.count").expect("cpu.count present");
        assert!(cpus.as_i64().is_some_and(|n| n >= 1));
        assert_eq!(kind("os.name"), Some(ValueKind::String));
        assert_eq!(kind("gpu.list"), Some(ValueKind::Seq));
        // An intermediate path is a map, an absent one resolves to nothing.
        assert_eq!(kind("cpu"), Some(ValueKind::Map));
        assert_eq!(kind("cpu.nonesuch"), None);
    }
}
