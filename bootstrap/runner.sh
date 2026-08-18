#!/bin/sh
#
# Setup Runner
# Smart environment setup based on Usage Profiles and System Tags.
#
# Logic:
# 1. Detect System Info (OS, Distro, Hardware).
# 2. Parse Arguments (--usage, --profile).
# 3. Iterate Stages: system -> apps -> user -> services.
# 4. In each stage, execute scripts matching:
#    - scope:<stage>
#    - usage:<current_usage> OR usage:common
#    - System constraints (os:*, gpu:*, etc.)
#

# Standard Safety
set -u

SETUP_FAILURES=$(mktemp)
cleanup() {
    rm -f "$SETUP_FAILURES"
    if command -v sudo >/dev/null 2>&1; then
        sudo -k
    fi
}
trap cleanup EXIT

# ==============================================================================
# 0. Configuration & Imports
# ==============================================================================

# Resolve Script Directory
resolve_script_dir() {
    _source="$0"
    [ -h "$0" ] && _source="$(readlink "$0")"
    cd -P "$(dirname "$_source")" && pwd
}

SCRIPT_DIR=$(resolve_script_dir)
# Assuming SCRIPT_DIR is .../setup, PROJECT_ROOT is .../scripts
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Load Libraries
# shellcheck source=../scripts/tags.sh
. "$PROJECT_ROOT/scripts/tags.sh"
# shellcheck source=../scripts/detect.sh
. "$PROJECT_ROOT/scripts/detect.sh"

# Default Context
SETUP_PROFILE="personal"
SETUP_USAGE="dev"
SETUP_HOSTNAME=""

# ==============================================================================
# 1. Argument Parsing
# ==============================================================================

show_help() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --profile <name>      Overlay / transcrypt context (default: personal)
  --usage <type>        Set usage type (e.g., dev, server) (default: dev)
  --hostname <name>     System hostname and Dotdrop host
  -h, --help            Show this help

Environment Variables:
  Required (Non-interactive):
    ROOT_PASSPHRASE     Root/Sudo password for unattended installation.
                        If not set in interactive mode, script will ask.
EOF
    # Dynamic Help Registration: Scan usages for help files or hooks
    # Convention: setup/usages/<usage>/HELP.md or similar
    # For now, we look for a common help text if available.
    if [ -d "$PROJECT_ROOT/setup/usages" ]; then
        printf "\nRegistered Usage Modules:\n"
        for usage_dir in "$PROJECT_ROOT/setup/usages"/*; do
            [ -d "$usage_dir" ] || continue
            usage_name=$(basename "$usage_dir")
            echo "  * $usage_name"

            # Check for optional env var definition file
            if [ -f "$usage_dir/ENV_VARS" ]; then
                echo "    Environment Variables for '$usage_name':"
                sed 's/^/      /' "$usage_dir/ENV_VARS"
            fi
        done
    fi

    cat <<EOF

Examples:
  ./setup.sh --profile work --usage server --hostname build-node-01
  ROOT_PASSPHRASE="secret" ./setup.sh
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --profile)
            SETUP_PROFILE="$2"
            shift 2
            ;;
        --usage)
            SETUP_USAGE="$2"
            shift 2
            ;;
        --hostname)
            SETUP_HOSTNAME="$2"
            shift 2
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            echo "Unknown argument: $1"
            show_help
            exit 1
            ;;
    esac
done

# ==============================================================================
# 2. Environment Detection
# ==============================================================================

CURRENT_OS=$(get_os)
CURRENT_DISTRO=""
if [ "$CURRENT_OS" = "linux" ]; then
    CURRENT_DISTRO=$(detect_distro)
fi
CURRENT_DE=$(detect_desktop)
GPU_VENDORS=$(detect_gpu_vendor)
CPU_VENDOR=$(detect_cpu_vendor)
IS_LAPTOP=0
if is_laptop; then IS_LAPTOP=1; fi

echo "[Setup] Context: OS=$CURRENT_OS Distro=$CURRENT_DISTRO Usage=$SETUP_USAGE Profile=$SETUP_PROFILE"

ASKPASS_SCRIPT="$PROJECT_ROOT/scripts/get-root-passphrase.sh"

if [ -z "${ROOT_PASSPHRASE:-}" ] && [ "$USER" != "root" ]; then
    echo "Error: ROOT_PASSPHRASE not set and running non-interactively."
    exit 1
fi

if [ -f "$ASKPASS_SCRIPT" ]; then
    export SUDO_ASKPASS="$ASKPASS_SCRIPT"
else
    echo "Error: Missing SUDO_ASKPASS script at ${ASKPASS_SCRIPT}"
    exit 1
fi

# Calculate Hostname
if [ -z "$SETUP_HOSTNAME" ]; then
    PREFIX=""
    if [ "$SETUP_PROFILE" != "personal" ]; then
        PREFIX="$SETUP_PROFILE-"
    fi
    if [ -z "$CURRENT_DISTRO" ]; then
        SUFFIX="$CURRENT_OS"
    else
        SUFFIX="$CURRENT_DISTRO"
    fi
    SETUP_HOSTNAME="${PREFIX}${USER}-${SUFFIX}"
fi

# Verify sudo access early
if ! sudo -A true; then
    echo "Error: Incorrect Password or 'sudo -A' failure."
    echo "Ensure SUDO_ASKPASS script is working correctly."
    exit 1
fi

export SETUP_PROFILE
export SETUP_USAGE
export SETUP_HOSTNAME
export PROJECTS_SCRIPT_DIR="$PROJECT_ROOT"

# ==============================================================================
# 3. Constraint Logic
# ==============================================================================

check_tag_constraint() {
    _tag="$1"

    # -- Usage Constraint --
    case "$_tag" in
        usage:*)
            _req_usage="${_tag#usage:}"
            # Pass if it's the requested usage OR 'common'
            if [ "$_req_usage" = "$SETUP_USAGE" ] || [ "$_req_usage" = "common" ]; then
                return 0
            fi
            return 1
            ;;
    esac

    # -- System Constraints --
    case "$_tag" in
        os:*)
            _req_os="${_tag#os:}"
            if [ "$_req_os" = "$CURRENT_OS" ]; then return 0; fi
            if [ "$CURRENT_OS" = "linux" ] && [ "$_req_os" = "$CURRENT_DISTRO" ]; then return 0; fi
            return 1 ;;
        gpu:any)
            [ -n "$GPU_VENDORS" ] && return 0
            return 1 ;;
        gpu:*)
            _req_gpu="${_tag#gpu:}"
            case "$GPU_VENDORS" in *"$_req_gpu"*) return 0 ;; esac
            return 1 ;;
        cpu:*)
            _req_cpu="${_tag#cpu:}"
            if [ "$_req_cpu" = "$CPU_VENDOR" ]; then return 0; fi
            return 1 ;;
        de:any)
            if is_desktop; then return 0; fi
            return 1 ;;
        de:*)
            _req_de="${_tag#de:}"
            if [ "$_req_de" = "$CURRENT_DE" ]; then return 0; fi
            return 1 ;;
        hw:laptop)
            [ "$IS_LAPTOP" -eq 1 ] && return 0
            return 1 ;;
        vps:*)
            _req_vps="${_tag#vps:}"
            if [ -z "${CURRENT_VPS:-}" ]; then
                . "$PROJECT_ROOT/scripts/detect_vps.sh"
                CURRENT_VPS=$(detect_vps)
            fi
            if [ "$_req_vps" = "$CURRENT_VPS" ]; then return 0; fi
            return 1 ;;
    esac

    # -- Dependency Constraint --
    case "$_tag" in
        dep:*)
            _cmd="${_tag#dep:}"
            if command -v "$_cmd" >/dev/null 2>&1; then return 0; fi
            return 1 ;;
    esac

    return 0
}

should_run_script() {
    _file="$1"
    _file_tags=$(tags_get "$_file")
    _os_constraint_seen=0
    _os_constraint_matched=0

    for _t in $_file_tags; do
        case "$_t" in
            os:*)
                _os_constraint_seen=1
                if check_tag_constraint "$_t"; then
                    _os_constraint_matched=1
                fi
                ;;
            usage:*|gpu:*|cpu:*|de:*|hw:*|dep:*|vps:*)
                if ! check_tag_constraint "$_t"; then
                    return 1
                fi
                ;;
        esac
    done

    if [ "$_os_constraint_seen" -eq 1 ] && [ "$_os_constraint_matched" -eq 0 ]; then
        return 1
    fi

    return 0
}

# ==============================================================================
# 4. Execution Loop
# ==============================================================================

SEARCH_ROOT="$SCRIPT_DIR/usages"
PHASES="system apps user services"

for PHASE in $PHASES; do
    echo "----------------------------------------------------------------"
    echo ">> Phase: $PHASE"
    echo "----------------------------------------------------------------"

    # Find candidate scripts via tags: scope:$PHASE
    # Sort key is tricky with find output.
    # We want to sort by FILENAME (e.g. 00-distro.sh), not path.
    # awk -F/ '{print $NF "\t" $0}' | sort -k1,1 | cut -f2-

    tags_find_all "$SEARCH_ROOT" "scope:$PHASE" | \
    awk -F/ '{print $NF "\t" $0}' | sort -k1,1 | cut -f2- | \
    while read -r script; do
        script_name=$(basename "$script")

        if should_run_script "$script"; then
            echo "[Running] $script"

            # Execute
            if [ -x "$script" ]; then
                "$script"
            else
                sh "$script"
            fi
            _status=$?
            if [ $_status -ne 0 ]; then
                echo "[Error] $script_name failed (Exit $_status)"
                printf '%s\n' "$script_name" >> "$SETUP_FAILURES"
            fi
        fi
    done
done

echo "----------------------------------------------------------------"
if [ -s "$SETUP_FAILURES" ]; then
    echo "[Setup] Failed modules:"
    sed 's/^/  - /' "$SETUP_FAILURES"
    exit 1
fi

echo "[Setup] Completed."
