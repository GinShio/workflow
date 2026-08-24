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

export PROJECTS_ROOT_DIR PROJECTS_SCRIPT_DIR
export DOTFILES_ROOT_DIR DOTFILES_OVERLAYS

# Load Libraries
# shellcheck source=../scripts/tags.sh
. "$PROJECTS_SCRIPT_DIR/scripts/tags.sh"
# shellcheck source=../scripts/detect.sh
. "$PROJECTS_SCRIPT_DIR/scripts/detect.sh"
# shellcheck source=../scripts/constraints.sh
. "$PROJECTS_SCRIPT_DIR/scripts/constraints.sh"

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
RUNNER_FAILURES=$(mktemp) || exit 1
RUNNER_CANDIDATES=$(mktemp) || {
    rm -f "$RUNNER_FAILURES"
    exit 1
}
cleanup_runner() {
    rm -f "$RUNNER_FAILURES" "$RUNNER_CANDIDATES"
}
trap cleanup_runner EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

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
            case "$_last_run" in
                ''|*[!0-9]*)
                    echo "Error: invalid schedule timestamp in $_state_file" >&2
                    return 2
                    ;;
            esac
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
                mkdir -p "$(dirname "$_state_file")"
                _state_tmp=$(mktemp "$STATE_DIR/.timestamp.XXXXXX") || return 1
                if date +%s > "$_state_tmp" && mv "$_state_tmp" "$_state_file"; then
                    return 0
                fi
                rm -f "$_state_tmp"
                return 1
                ;;
        esac
    done
}

# ==============================================================================
# 3. Filtering Logic
# ==============================================================================

service_constraint() {
    _sc_tag=$1
    _sc_file=$2

    case "$_sc_tag" in
        power:ac)
            [ "$IS_ON_AC" -eq 1 ]
            ;;
        power:battery)
            [ "$IS_ON_AC" -eq 0 ]
            ;;
        power:any)
            return 0
            ;;
        schedule:*)
            check_schedule_constraint "$_sc_tag" "$_sc_file"
            ;;
        *)
            return 2
            ;;
    esac
}

should_run_script() {
    constraints_match_file "$1" service_constraint
}


# ==============================================================================
# 4. Main Execution Loop
# ==============================================================================

echo "[Runner] scanning modules for $TARGET_TYPE..."

# A malformed tag is not a skippable module: an unknown `hw:`/`gpu:` value
# reads as an unmet constraint further down, so a typo would silently drop the
# module instead of failing here.
constraints_validate_tree "$MODULES_DIR" || exit 1
tags_find_all "$MODULES_DIR" "$TARGET_TYPE" |
    awk -F/ '{print $NF "\t" $0}' |
    sort -k1,1 -k2,2 |
    cut -f2- > "$RUNNER_CANDIDATES"

while IFS= read -r script; do
    [ -n "$script" ] || continue
    script_name=$(basename "$script")

    should_run_script "$script"
    _match_status=$?
    case "$_match_status" in
        0)
            echo "[Runner] executing: $script_name"
            sh -eu "$script"
            _status=$?
            if [ "$_status" -ne 0 ]; then
                echo "[Runner] warning: $script_name exited with error $_status" >&2
                printf '%s\n' "$script_name" >> "$RUNNER_FAILURES"
            elif ! update_schedule_timestamp "$script"; then
                echo "[Runner] warning: could not record schedule for $script_name" >&2
                printf '%s\n' "$script_name (schedule state)" >> "$RUNNER_FAILURES"
            fi
            ;;
        1)
            ;;
        *)
            echo "[Runner] error: could not evaluate constraints for $script_name" >&2
            printf '%s\n' "$script_name (constraints)" >> "$RUNNER_FAILURES"
            ;;
    esac
done < "$RUNNER_CANDIDATES"

if [ -s "$RUNNER_FAILURES" ]; then
    echo "[Runner] failed modules:" >&2
    while IFS= read -r failed; do
        printf '  - %s\n' "$failed" >&2
    done < "$RUNNER_FAILURES"
    exit 1
fi

echo "[Runner] finished successfully."
