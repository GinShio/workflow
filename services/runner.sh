#!/bin/sh
#
# Service Runner
# Smart scheduler that executes scripts based on tags and system environment.
# Usage: ./runner.sh <type>
# Example: ./runner.sh autostart
#

set -u

# ==============================================================================
# 1. Initialization
# ==============================================================================

if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/wits/.env" ]; then
    . "${XDG_CONFIG_HOME:-$HOME/.config}/wits/.env"
fi

# Resolve the directory containing this script without relying on GNU
# `readlink -f` (unavailable on macOS and some BSDs).
resolve_script_dir() {
    _source=$0

    case "$_source" in
        */*) ;;
        *)
            _resolved=$(command -v "$_source" 2>/dev/null || true)
            [ -n "$_resolved" ] && _source=$_resolved
            ;;
    esac

    while [ -h "$_source" ]; do
        _source_dir=$(CDPATH= cd -P "$(dirname "$_source")" 2>/dev/null && pwd) || return 1
        _source=$(readlink "$_source") || return 1
        case "$_source" in
            /*) ;;
            *) _source="$_source_dir/$_source" ;;
        esac
    done

    CDPATH= cd -P "$(dirname "$_source")" 2>/dev/null && pwd
}

# Fallback block if variables are unset
if [ -z "${PROJECTS_SCRIPT_DIR:-}" ]; then
    _script_dir=$(resolve_script_dir) || {
        echo "Error: unable to resolve service runner directory" >&2
        exit 1
    }
    PROJECTS_SCRIPT_DIR=$(dirname "$_script_dir")
fi

# Fix detection usage spam
if [ "${1:-}" = "detect_usage_fix" ]; then return 0; fi

export PROJECTS_ROOT_DIR PROJECTS_SCRIPT_DIR
export DOTFILES_ROOT_DIR DOTFILES_OVERLAYS

# Load Libraries
# shellcheck source=../scripts/tags.sh
. "$PROJECTS_SCRIPT_DIR/scripts/tags.sh"
# shellcheck source=../scripts/detect.sh
. "$PROJECTS_SCRIPT_DIR/scripts/detect.sh"

# State Directory for Scheduling
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/workflow/services/timestamps"
mkdir -p "$STATE_DIR"

# Validate Input
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <type>" >&2
    echo "  <type>: e.g., 'autostart', 'nightly'" >&2
    exit 1
fi

TARGET_TYPE="type:$1"
MODULES_DIR="$PROJECTS_SCRIPT_DIR/services/modules"

# ==============================================================================
# 2. Environment Detection
# ==============================================================================

# Gather System Info
CURRENT_OS=$(get_os)         # linux, darwin, freebsd...
CURRENT_DISTRO=""
if [ "$CURRENT_OS" = "linux" ]; then
    CURRENT_DISTRO=$(detect_distro) # opensuse, debian...
fi

# Desktop Environment Detection
CURRENT_DE=$(detect_desktop) # gnome, kde, sway, headless...

# Hardware Detection via detect.sh
GPU_VENDORS=$(detect_gpu_vendor)
CPU_VENDOR=$(detect_cpu_vendor)

IS_LAPTOP=0
if is_laptop; then IS_LAPTOP=1; fi

IS_ON_AC=0
if is_on_ac; then IS_ON_AC=1; fi

# ==============================================================================
# Helper: Scheduling Functionality
# ==============================================================================

# Helper: Get state file path
hash_schedule_key() {
    _value=$1

    if command -v md5sum >/dev/null 2>&1; then
        printf '%s' "$_value" | md5sum | awk '{print $1}'
    elif command -v md5 >/dev/null 2>&1; then
        printf '%s' "$_value" | md5 -q
    elif command -v openssl >/dev/null 2>&1; then
        printf '%s' "$_value" | openssl dgst -md5 | awk '{print $NF}'
    else
        printf '%s' "$_value" | cksum | awk '{print $1 "-" $2}'
    fi
}

get_schedule_state_file() {
    _script_path="$1"
    _hash=$(hash_schedule_key "$_script_path")
    printf '%s\n' "$STATE_DIR/$_hash"
}

# Helper: Check scheduling logic
# Returns 0 (allow execute) or 1 (skip)
check_schedule_constraint() {
    _tag="$1"
    _script_path="$2"

    # Prefix: schedule:*
    case "$_tag" in
        schedule:*)
            _interval="${_tag#schedule:}"
            _state_file=$(get_schedule_state_file "$_script_path")

            # First run detection
            if [ ! -f "$_state_file" ]; then
                return 0
            fi

            _last_run=$(cat "$_state_file")
            _now=$(date +%s)

            # Parse Interval
            _unit=""
            _val=""

            # Keywords
            case "$_interval" in
                daily)   _val=1; _unit="d" ;;
                weekly)  _val=7; _unit="d" ;;
                monthly) _val=30; _unit="d" ;;
                *)
                    # Custom (Xd, Xh, Xm)
                    if echo "$_interval" | grep -q 'd$'; then
                        _unit="d"
                        _val=$(echo "$_interval" | tr -d 'd')
                    elif echo "$_interval" | grep -q 'h$'; then
                        _unit="h"
                        _val=$(echo "$_interval" | tr -d 'h')
                    elif echo "$_interval" | grep -q 'm$'; then
                        _unit="m"
                        _val=$(echo "$_interval" | tr -d 'm')
                    else
                        # Fallback/Unknown -> Allow run or deny? Let's allow but warn?
                        # For safety, treat unknown as no constraint
                        return 0
                    fi
                    ;;
            esac

            # Logic Switch: Calendar (d) vs Strict (h/m)
            if [ "$_unit" = "d" ]; then
                # Calendar Day Calculation
                # We normalize to midnight to count "days passed"
                # Using pure POSIX shell arithmetic $((...))
                # https://pubs.opengroup.org/onlinepubs/9699919799/utilities/V3_chap02.html#tag_18_06_04

                # Note: This calculates UTC midnight, ignoring local timezone offsets.
                # However, since we compare consistent "days since epoch", this is
                # mathematically consistent for interval checks (every X days).
                # The "day boundary" will just be 00:00 UTC instead of local time.

                # Floor to start of day (00:00:00 UTC)
                _last_midnight=$(( _last_run - (_last_run % 86400) ))
                _curr_midnight=$(( _now - (_now % 86400) ))

                _diff_sec=$(( _curr_midnight - _last_midnight ))
                _diff_days=$(( _diff_sec / 86400 ))

                if [ "$_diff_days" -ge "$_val" ]; then
                     return 0
                fi
                return 1

            else
                # Strict Time Calculation (Hours/Minutes)
                _diff_sec=$(( _now - _last_run ))
                _target_sec=0

                if [ "$_unit" = "h" ]; then
                    _target_sec=$(( _val * 3600 ))
                elif [ "$_unit" = "m" ]; then
                    _target_sec=$(( _val * 60 ))
                fi

                if [ "$_diff_sec" -ge "$_target_sec" ]; then
                     return 0
                fi
                return 1
            fi
            ;;
    esac

    # Not a schedule tag or unrecognized -> Allow
    return 0
}

# Helper: Update schedule timestamp
update_schedule_timestamp() {
    _script_path="$1"

    _file_tags=$(tags_get "$_script_path")
    for _t in $_file_tags; do
        case "$_t" in
            schedule:*)
                _state_file=$(get_schedule_state_file "$_script_path")
                # Ensure directory exists just in case
                mkdir -p "$(dirname "$_state_file")"
                date +%s > "$_state_file"
                return 0
                ;;
        esac
    done
}

# ==============================================================================
# 3. Filtering Logic
# ==============================================================================

# Helper: Check if a script's specific tag matches current environment
# Returns 0 (pass) or 1 (skip)
check_tag_constraint() {
    _tag="$1"
    _file="$2"

    # Prefix: schedule:* (Time-based Scheduling)
    case "$_tag" in
        schedule:*)
            if ! check_schedule_constraint "$_tag" "$_file"; then
                return 1
            fi
            ;;
    esac

    # Prefix: os:*
    case "$_tag" in
        os:*)
            _req_os="${_tag#os:}"
            # Check OS Family
            if [ "$_req_os" = "$CURRENT_OS" ]; then return 0; fi
            # Check Distro (Linux specific)
            if [ "$CURRENT_OS" = "linux" ] && [ "$_req_os" = "$CURRENT_DISTRO" ]; then return 0; fi
            return 1
            ;;
    esac

    # Prefix: power:* (Power Source)
    case "$_tag" in
        power:ac)
            # Require AC Power
            if [ "$IS_ON_AC" -eq 1 ]; then return 0; fi
            return 1 ;;
        power:battery)
            # Require Battery Power (Not AC)
            if [ "$IS_ON_AC" -eq 0 ]; then return 0; fi
            return 1 ;;
        power:any)
            return 0 ;;
    esac

    # Prefix: gpu:* (GPU Vendor/Presence)
    case "$_tag" in
        gpu:any)
            if [ -n "$GPU_VENDORS" ]; then return 0; fi
            return 1 ;;
        gpu:*)
            _req_gpu="${_tag#gpu:}"
            case "$GPU_VENDORS" in *"$_req_gpu"*) return 0 ;; esac
            return 1 ;;
    esac

    # Prefix: cpu:* (CPU Vendor)
    case "$_tag" in
        cpu:*)
            _req_cpu="${_tag#cpu:}"
            if [ "$_req_cpu" = "$CPU_VENDOR" ]; then return 0; fi
            return 1 ;;
    esac

    # Prefix: de:* (Desktop Environment)
    case "$_tag" in
        de:*)
            _req_de="${_tag#de:}"
            # Multi-value check? For now simple exact match or substring
            if [ "$_req_de" = "$CURRENT_DE" ]; then return 0; fi
            return 1 ;;
    esac

    # Prefix: hw:* (Other Hardware)
    case "$_tag" in
        hw:laptop)
            [ "$IS_LAPTOP" -eq 1 ] && return 0
            return 1 ;;
    esac

    # Prefix: dep:* (Dependency Check)
    case "$_tag" in
        dep:*)
            _cmd="${_tag#dep:}"
            if command -v "$_cmd" >/dev/null 2>&1; then return 0; fi
            return 1
            ;;
    esac

    # Tag is not a constraint type we know, so it doesn't block execution.
    return 0
}

# Helper: Decide if script should run based on ALL its tags
should_run_script() {
    _file="$1"
    _file_tags=$(tags_get "$_file")

    # Iterate over all tags in the file
    for _t in $_file_tags; do

        # Identify constraint tags
        # We explicitly list all prefixes that impose an execution constraint
        case "$_t" in
            # Add schedule:* here to ensure it's caught
            os:*|hw:*|dep:*|gpu:*|cpu:*|de:*|power:*|schedule:*)
                if ! check_tag_constraint "$_t" "$_file"; then
                    # Constraint failed -> Skip execution
                    return 1
                fi
                ;;
            # Future expansion for env:* (e.g. kde/gnome) can go here
        esac
    done

    # All constraints passed (or none existed)
    return 0
}


# ==============================================================================
# 4. Main Execution Loop
# ==============================================================================

echo "[Runner] scanning modules for $TARGET_TYPE..."

# 1. Find candidates (files matching the requested type)
# We use sort to ensure deterministic order (NN-name.sh) based on filename
# Sort by filename (field 1) then by full path (field 2) to handle same-named files deterministically
tags_find_all "$MODULES_DIR" "$TARGET_TYPE" | awk -F/ '{print $NF "\t" $0}' | sort -k1,1 -k2,2 | cut -f2- | while read -r script; do

    script_name=$(basename "$script")

if should_run_script "$script"; then
    echo "[Runner] executing: $script_name"

    # 3. Execute
    # Run in subshell to protect runner's environment
    (
        # Pass the trigger type as argument just in case
        sh "$script"
    )
    _status=$?

    if [ $_status -ne 0 ]; then
        echo "[Runner] warning: $script_name exited with error $_status" >&2
    else
        # Only update schedule on success
        update_schedule_timestamp "$script"
    fi
    else
        # Verbose debug (optional, currently commented out)
        # echo "[Runner] skipping: $script_name (constraints unmet)"
        :
    fi
done

echo "[Runner] finished."
