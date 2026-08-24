#!/bin/sh
# System Detection Logic

get_os() {
    uname -s | tr '[:upper:]' '[:lower:]'
}

# Check if the system is a laptop
# Behavior:
#   Detects if the current system is a portable device (Laptop, Notebook, etc.)
# Returns:
#   0 (True)  - If the system is identified as a laptop/portable.
#   1 (False) - If the system is a desktop, server, or type could not be determined.
# Usage:
#   if is_laptop; then echo "It's a laptop"; fi
is_laptop() {
    _os=$(get_os)

    if [ "$_os" = "linux" ]; then
        # Method 1: Check for battery in /sys/class/power_supply/
        for bat in /sys/class/power_supply/BAT* /sys/class/power_supply/battery; do
            if [ -e "$bat/type" ] && grep -qi "battery" "$bat/type" 2>/dev/null; then
                return 0
            fi
        done

        # Method 2: Check DMI chassis type (portable/laptop types: 8, 9, 10, 14, 30, 31, 32)
        # 8=Portable, 9=Laptop, 10=Notebook, 14=Sub Notebook, 30=Tablet, 31=Convertible, 32=Detachable
        if [ -r /sys/class/dmi/id/chassis_type ]; then
            chassis_type=$(cat /sys/class/dmi/id/chassis_type 2>/dev/null)
            case "$chassis_type" in
                8|9|10|14|30|31|32) return 0 ;;
            esac
        fi

    elif [ "$_os" = "darwin" ]; then
        # macOS: sysctl hw.model
        # MacBook*
        _model=$(sysctl -n hw.model 2>/dev/null)
        case "$_model" in
            MacBook*) return 0 ;;
        esac

    elif [ "$_os" = "freebsd" ] || [ "$_os" = "openbsd" ]; then
        # FreeBSD/OpenBSD: sysctl hw.machine_arch or hw.model to guess?
        # Better: use apm to check for battery presence
        if command -v apm >/dev/null 2>&1; then
             # apm -b: battery status. If not 255 (unknown/no battery), likely a laptop
             # Or check output for "Battery"
             _batt=$(apm -b 2>/dev/null)
             if [ "$_batt" != "255" ] && [ -n "$_batt" ]; then
                return 0
             fi
        fi
    fi

    return 1
}

# Check if the system is currently running on AC power (Mains)
# Behavior:
#   Detects if the system is plugged into a power source.
#   Desktops/Servers generally satisfy this by default.
# Returns:
#   0 (True)  - System is on AC power (or is a Desktop).
#   1 (False) - System is running on Battery.
# Usage:
#   if is_on_ac; then echo "High power mode"; fi
is_on_ac() {
    _os=$(get_os)

    # If not a laptop, assume always on AC (Desktop/Server)
    # Note: is_laptop check might need OS specific implementation too,
    # but for now we trust it or add minimal checks below
    if ! is_laptop; then return 0; fi

    if [ "$_os" = "linux" ]; then
        # Check for AC adapter online status
        # Linux kernel usually exposes this via /sys/class/power_supply
        for supply in /sys/class/power_supply/*; do
            if [ -r "$supply/type" ] && grep -qi "Mains" "$supply/type"; then
                if [ -r "$supply/online" ] && grep -q "1" "$supply/online"; then
                    return 0
                fi
            # Some USB-C power supplies might show up as USB
            elif [ -r "$supply/type" ] && grep -qi "USB" "$supply/type"; then
                 if [ -r "$supply/online" ] && grep -q "1" "$supply/online"; then
                    # Additional check: make sure it's delivering power, not just data
                    # But typically 'online=1' for a power_supply means power.
                    return 0
                fi
            fi
        done
        # If no online AC adapter found, we are on battery
        return 1

    elif [ "$_os" = "darwin" ]; then
        # macOS: pmset -g batt
        # Output example: " -InternalBattery-0 (id=...)"... "AC Power" or "Battery Power"
        if pmset -g batt | grep -q 'AC Power'; then
            return 0
        fi
        return 1

    elif [ "$_os" = "freebsd" ] || [ "$_os" = "openbsd" ]; then
        # FreeBSD/OpenBSD: apm command
        # apm -a: 0=offline, 1=online. Returns 1 if AC is on.
        if command -v apm >/dev/null 2>&1; then
            _status=$(apm -a 2>/dev/null)
            if [ "$_status" -eq 1 ]; then
                return 0
            fi
        fi
        return 1
    fi

    # Fallback for unknown OS: assumed AC to be safe, or Battery?
    # Assume AC to avoid blocking scripts unnecessarily
    return 0
}

detect_arch() {
    _arch=$(uname -m | tr '[:upper:]' '[:lower:]')
    case "$_arch" in
        x86_64|amd64) echo "x86_64" ;;
        i*86) echo "x86" ;;
        aarch64|arm64) echo "arm64" ;;
        arm*) echo "arm" ;;
        riscv64) echo "riscv64" ;;
        *) echo "$_arch" ;;
    esac
}

detect_cpu_vendor() {
    _os=$(get_os)
    _cpu_vendor="unknown"

    if [ "$_os" = "linux" ]; then
        if [ -r /proc/cpuinfo ]; then
            if grep -q "GenuineIntel" /proc/cpuinfo; then
                _cpu_vendor="intel"
            elif grep -q "AuthenticAMD" /proc/cpuinfo; then
                _cpu_vendor="amd"
            elif grep -q "SiFive" /proc/cpuinfo; then
                _cpu_vendor="sifive"
            elif grep -q "T-Head" /proc/cpuinfo; then
                _cpu_vendor="thead"
            elif grep -q "StarFive" /proc/cpuinfo; then
                _cpu_vendor="starfive"
            else
                _impl=$(grep -i "^CPU implementer" /proc/cpuinfo | head -n 1 | cut -d: -f2 | tr -d ' \t' | tr '[:upper:]' '[:lower:]')
                case "$_impl" in
                    0x41) _cpu_vendor="arm" ;;
                    0x42) _cpu_vendor="broadcom" ;;
                    0x43) _cpu_vendor="cavium" ;;
                    0x48) _cpu_vendor="hisilicon" ;;
                    0x4e) _cpu_vendor="nvidia" ;;
                    0x50) _cpu_vendor="ampere" ;;
                    0x51) _cpu_vendor="qualcomm" ;;
                    0x53) _cpu_vendor="samsung" ;;
                    0x61) _cpu_vendor="apple" ;;
                    *)
                        if grep -q "BCM" /proc/cpuinfo; then
                            _cpu_vendor="broadcom"
                        fi
                        ;;
                esac
            fi
        fi
    elif [ "$_os" = "darwin" ]; then
        _brand=$(sysctl -n machdep.cpu.brand_string 2>/dev/null | tr '[:upper:]' '[:lower:]')
        if echo "$_brand" | grep -q "intel"; then _cpu_vendor="intel"; fi
        if echo "$_brand" | grep -q "apple"; then _cpu_vendor="apple"; fi
    elif [ "$_os" = "freebsd" ] || [ "$_os" = "openbsd" ]; then
         _brand=$(sysctl -n hw.model 2>/dev/null | tr '[:upper:]' '[:lower:]')
         if echo "$_brand" | grep -q "intel"; then _cpu_vendor="intel"; fi
         if echo "$_brand" | grep -q "amd"; then _cpu_vendor="amd"; fi
    fi
    echo "$_cpu_vendor"
}

detect_gpu_vendor() {
    _vendors=""
    _os=$(get_os)

    if [ "$_os" = "linux" ]; then
        if [ -d "/sys/class/drm" ]; then
            for _card in /sys/class/drm/card*; do
                [ -e "$_card/device/driver" ] || continue
                if command -v readlink >/dev/null 2>&1; then
                    _driver_path=$(readlink -f "$_card/device/driver")
                    _driver=$(basename "$_driver_path")
                else
                    continue
                fi

                case "$_driver" in
                    amdgpu|radeon) _v="amd" ;;
                    i915|xe|iris)  _v="intel" ;;
                    nvidia|nvidia-drm) _v="nvidia" ;;
                    vc4|v3d)       _v="videocore" ;;
                    panfrost|mali) _v="mali" ;;
                    tegra*|nv*)    _v="nvidia" ;;
                    virtio_gpu)    _v="virtio" ;;
                    *)             _v="" ;;
                esac

                if [ -n "$_v" ]; then
                     case " $_vendors " in
                        *" $_v "*) ;;
                        *) _vendors="$_vendors $_v" ;;
                     esac
                fi
            done
        fi

        # sysfs only exposes devices with a bound driver.  Merge PCI evidence so
        # a powered-down or unbound discrete GPU on a hybrid system is not lost.
        # One awk pass; the END block fixes the order so repeated runs agree.
        if command -v lspci >/dev/null 2>&1; then
            for _v in $(lspci -mm 2>/dev/null | awk '
                {
                    line = tolower($0)
                    if (line !~ /vga|3d|display/) next
                    if (line ~ /nvidia/)   seen["nvidia"] = 1
                    if (line ~ /amd|ati/)  seen["amd"] = 1
                    if (line ~ /intel/)    seen["intel"] = 1
                }
                END {
                    if ("nvidia" in seen) print "nvidia"
                    if ("amd" in seen)    print "amd"
                    if ("intel" in seen)  print "intel"
                }
            '); do
                case " $_vendors " in
                    *" $_v "*) ;;
                    *) _vendors="$_vendors $_v" ;;
                esac
            done
        fi

    elif [ "$_os" = "darwin" ]; then
        _profile=$(system_profiler SPDisplaysDataType 2>/dev/null)
        if echo "$_profile" | grep -iq "NVIDIA"; then _vendors="$_vendors nvidia"; fi
        if echo "$_profile" | grep -iq "AMD"; then _vendors="$_vendors amd"; fi
        if echo "$_profile" | grep -iq "Intel"; then _vendors="$_vendors intel"; fi
        if echo "$_profile" | grep -iq "Apple"; then _vendors="$_vendors apple"; fi
    fi
    echo "$_vendors" | awk '{$1=$1};1'
}

detect_platform() {
    _platform="generic"
    if [ -f /proc/device-tree/model ]; then
        _platform=$(tr -d '\0' < /proc/device-tree/model)
    elif [ -f /sys/firmware/devicetree/base/model ]; then
        _platform=$(tr -d '\0' < /sys/firmware/devicetree/base/model)
    elif [ -f /sys/class/dmi/id/product_name ]; then
        _platform=$(cat /sys/class/dmi/id/product_name)
    elif [ "$(uname -s)" = "Darwin" ]; then
         _platform=$(sysctl -n hw.model)
    fi
    echo "$_platform"
}

detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        # Normalize opensuse variants (leap, tumbleweed) to just "opensuse"
        case "$ID" in
            opensuse*) echo "opensuse" ;;
            *) echo "$ID" ;;
        esac
    else
        echo "unknown"
    fi
}

detect_distro_version() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        echo "$VERSION_ID"
    else
        echo "unknown"
    fi
}

detect_kernel() {
    uname -r
}

detect_kernel_major() {
    uname -r | cut -d. -f1
}

detect_kernel_minor() {
    uname -r | cut -d. -f2
}

detect_kernel_patch() {
    # Handles cases like 6.6.10-arch1-1 -> 10
    uname -r | cut -d. -f3 | cut -d- -f1
}

detect_cmdline() {
    if [ -f /proc/cmdline ]; then
        cat /proc/cmdline
    else
        echo ""
    fi
}

has_cmdline() {
    if [ -f /proc/cmdline ]; then
        if grep -q "$1" /proc/cmdline; then
            echo "true"
        else
            echo "false"
        fi
    else
        echo "false"
    fi
}

detect_memory_mb() {
    _os=$(get_os)
    if [ "$_os" = "linux" ]; then
        if [ -f /proc/meminfo ]; then
            awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo
        fi
    elif [ "$_os" = "darwin" ]; then
        _mem=$(sysctl -n hw.memsize 2>/dev/null)
        echo $(( _mem / 1024 / 1024 ))
    elif [ "$_os" = "freebsd" ] || [ "$_os" = "openbsd" ]; then
        _mem=$(sysctl -n hw.physmem 2>/dev/null)
        echo $(( _mem / 1024 / 1024 ))
    else
        echo "0"
    fi
}

detect_hostname() {
    uname -n | tr '[:upper:]' '[:lower:]'
}

detect_desktop() {
    # Check common environment variables
    _de="${XDG_CURRENT_DESKTOP:-${DESKTOP_SESSION:-}}"
    _de=$(echo "$_de" | tr '[:upper:]' '[:lower:]')

    if [ -n "$_de" ]; then
        # Normalize some common values
        case "$_de" in
            *gnome*) echo "gnome" ;;
            *kde*|*plasma*) echo "kde" ;;
            *xfce*) echo "xfce" ;;
            *mate*) echo "mate" ;;
            *cinnamon*) echo "cinnamon" ;;
            *lxqt*) echo "lxqt" ;;
            *sway*) echo "sway" ;;
            *hyprland*) echo "hyprland" ;;
            *i3*) echo "i3" ;;
            *pantheon*) echo "pantheon" ;;
            *budgie*) echo "budgie" ;;
            *) echo "$_de" ;;
        esac
        return 0
    fi

    # Fallback: Inference from processes
    if pgrep -x "kwin_wayland" >/dev/null || pgrep -x "kwin_x11" >/dev/null || pgrep -x "plasmashell" >/dev/null; then
        echo "kde"
    elif pgrep -x "gnome-shell" >/dev/null; then
        echo "gnome"
    elif pgrep -x "xfce4-session" >/dev/null; then
        echo "xfce"
    elif pgrep -x "lxqt-session" >/dev/null; then
        echo "lxqt"
    elif pgrep -x "mate-session" >/dev/null; then
        echo "mate"
    elif pgrep -x "cinnamon-session" >/dev/null; then
        echo "cinnamon"
    elif pgrep -x "sway" >/dev/null; then
        echo "sway"
    elif pgrep -x "hyprland" >/dev/null; then
        echo "hyprland"
    elif pgrep -x "i3" >/dev/null; then
        echo "i3"
    elif pgrep -x "bspwm" >/dev/null; then
        echo "bspwm"
    elif pgrep -x "awesome" >/dev/null; then
        echo "awesome"
    else
        echo "headless"
    fi
}

is_desktop() {
    [ "$(detect_desktop)" != "headless" ]
}

detect_display_server() {
    _ds="unknown"
    # pam_systemd exports the session's type, which is what distinguishes an
    # X11 client under Xwayland (wayland) from a real X server (x11) — the
    # question DISPLAY alone cannot answer. Any other value, `tty` included,
    # falls through so the vocabulary here stays wayland/x11/unknown.
    case "${XDG_SESSION_TYPE:-}" in
        wayland) _ds="wayland" ;;
        x11)     _ds="x11" ;;
        *)
            if [ -n "${WAYLAND_DISPLAY:-}" ]; then
                _ds="wayland"
            elif [ -n "${DISPLAY:-}" ]; then
                _ds="x11"
            fi
            ;;
    esac
     # Fallback for some non-systemd or weird envs
    if [ "$_ds" = "unknown" ]; then
         if pgrep -x "Xorg" >/dev/null || pgrep -x "X" >/dev/null; then
             _ds="x11"
         fi
    fi
    echo "$_ds"
}

# Dispatcher
# Provide execution if script is run directly, allow sourcing as library
if [ "${0##*/}" = "detect.sh" ] && [ "$#" -gt 0 ]; then
    case "$1" in
        arch) detect_arch ;;
        cpu) detect_cpu_vendor ;;
        gpu) detect_gpu_vendor ;;
        mem|memory_mb) detect_memory_mb ;;
        host|hostname) detect_hostname ;;
        de|desktop) detect_desktop ;;
        is_desktop) is_desktop && echo "true" || echo "false" ;;
        ds|display_server) detect_display_server ;;
        platform) detect_platform ;;
        distro) detect_distro ;;
        distro_ver) detect_distro_version ;;
        kernel) detect_kernel ;;
        kernel_major) detect_kernel_major ;;
        kernel_minor) detect_kernel_minor ;;
        kernel_patch) detect_kernel_patch ;;
        cmdline) detect_cmdline ;;
        has_cmdline) has_cmdline "$2" ;;
        *)
            echo "Usage: $0 {arch|cpu|gpu|platform|distro|distro_ver|kernel|cmdline|has_cmdline <arg>}" >&2
            exit 1
            ;;
    esac
fi
