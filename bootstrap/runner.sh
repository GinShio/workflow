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
    if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then
        sudo -k
    fi
}
trap cleanup EXIT

# ==============================================================================
# 0. Configuration & Imports
# ==============================================================================

# Resolve Script Directory
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
        _source_dir=$(CDPATH= cd -P "$(dirname "$_source")" 2>/dev/null && pwd) ||
            return 1
        _source=$(readlink "$_source") || return 1
        case "$_source" in
            /*) ;;
            *) _source="$_source_dir/$_source" ;;
        esac
    done
    CDPATH= cd -P "$(dirname "$_source")" 2>/dev/null && pwd
}

SCRIPT_DIR=$(resolve_script_dir) || {
    echo "Error: unable to resolve bootstrap runner directory" >&2
    exit 1
}
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Load Libraries
# shellcheck source=../scripts/tags.sh
. "$PROJECT_ROOT/scripts/tags.sh"
# shellcheck source=../scripts/detect.sh
. "$PROJECT_ROOT/scripts/detect.sh"
# shellcheck source=../scripts/constraints.sh
. "$PROJECT_ROOT/scripts/constraints.sh"

# Default Context
SETUP_PROFILE="personal"
SETUP_USAGE="dev"
SETUP_HOSTNAME=""

# ==============================================================================
# 1. Argument Parsing
# ==============================================================================

show_help() {
    cat <<EOF
Usage: bootstrap/$(basename "$0") [OPTIONS]

Options:
  --profile <name>      Overlay / transcrypt context (default: personal)
  --usage <type>        Set usage type: dev or vps (default: dev)
  --hostname <name>     System hostname and Dotdrop host
  -h, --help            Show this help

Environment Variables:
  Required for non-root dev setup:
    ROOT_PASSPHRASE     Root/Sudo password for unattended installation.
EOF
    if [ -d "$SCRIPT_DIR/usages" ]; then
        printf "\nRegistered Usage Modules:\n"
        for usage_dir in "$SCRIPT_DIR/usages"/*; do
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
  ROOT_PASSPHRASE="secret" ./bootstrap/runner.sh --usage dev
  ./bootstrap/runner.sh --profile personal --usage vps --hostname edge-01
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --profile)
            [ "$#" -ge 2 ] || {
                echo "Error: --profile requires a value" >&2
                exit 1
            }
            SETUP_PROFILE="$2"
            shift 2
            ;;
        --usage)
            [ "$#" -ge 2 ] || {
                echo "Error: --usage requires a value" >&2
                exit 1
            }
            SETUP_USAGE="$2"
            shift 2
            ;;
        --hostname)
            [ "$#" -ge 2 ] || {
                echo "Error: --hostname requires a value" >&2
                exit 1
            }
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

case "$SETUP_USAGE" in
    dev|vps) ;;
    *)
        echo "Error: unsupported usage '$SETUP_USAGE' (expected dev or vps)" >&2
        exit 1
        ;;
esac

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
CURRENT_USER=$(id -un)
IS_LAPTOP=0
if is_laptop; then IS_LAPTOP=1; fi

echo "[Setup] Context: OS=$CURRENT_OS Distro=$CURRENT_DISTRO Usage=$SETUP_USAGE Profile=$SETUP_PROFILE"

ASKPASS_SCRIPT="$PROJECT_ROOT/scripts/get-root-passphrase.sh"

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
    SETUP_HOSTNAME="${PREFIX}${CURRENT_USER}-${SUFFIX}"
fi

if [ "$SETUP_USAGE" = vps ]; then
    if [ "$(id -u)" -ne 0 ]; then
        echo "Error: VPS bootstrap changes the base system and must run as root." >&2
        exit 1
    fi
elif [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 || {
        echo "Error: sudo is required for dev bootstrap." >&2
        exit 1
    }
    [ -n "${ROOT_PASSPHRASE:-}" ] || {
        echo "Error: ROOT_PASSPHRASE is required for unattended dev bootstrap." >&2
        exit 1
    }
    [ -x "$ASKPASS_SCRIPT" ] || {
        echo "Error: Missing executable SUDO_ASKPASS script at ${ASKPASS_SCRIPT}" >&2
        exit 1
    }
    export SUDO_ASKPASS="$ASKPASS_SCRIPT"
    if ! sudo -A true; then
        echo "Error: sudo askpass authentication failed." >&2
        exit 1
    fi
fi

export SETUP_PROFILE
export SETUP_USAGE
export SETUP_HOSTNAME
export PROJECTS_SCRIPT_DIR="$PROJECT_ROOT"

# ==============================================================================
# 3. Constraint Logic
# ==============================================================================

bootstrap_constraint() {
    _bc_tag=$1

    case "$_bc_tag" in
        usage:*)
            _bc_usage=${_bc_tag#usage:}
            # The historical "common" tree is the workstation baseline shared
            # by supported desktop OSes. VPS has its own complete, root-only
            # baseline and must not inherit workstation packages.
            if [ "$_bc_usage" = common ]; then
                [ "$SETUP_USAGE" = dev ]
            else
                [ "$_bc_usage" = "$SETUP_USAGE" ]
            fi
            ;;
        vps:*)
            if [ -z "${CURRENT_VPS:-}" ]; then
                # shellcheck source=../scripts/detect_vps.sh
                . "$PROJECT_ROOT/scripts/detect_vps.sh"
                CURRENT_VPS=$(detect_vps)
            fi
            [ "${_bc_tag#vps:}" = "$CURRENT_VPS" ]
            ;;
        *)
            return 2
            ;;
    esac
}

should_run_script() {
    constraints_match_file "$1" bootstrap_constraint
}

# ==============================================================================
# 4. Execution Loop
# ==============================================================================

SEARCH_ROOT="$SCRIPT_DIR/usages"
PHASES="system apps user services"

# A malformed tag is not a skippable module: an unknown `hw:`/`gpu:` value
# reads as an unmet constraint further down, so a typo would silently drop the
# module instead of failing here.
constraints_validate_tree "$SEARCH_ROOT" || exit 1

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

        should_run_script "$script"
        _match_status=$?
        case "$_match_status" in
            0)
                echo "[Running] $script"
                # Tagged modules are a POSIX-shell contract. Enforce fail-fast
                # semantics even when an older module lacks its own `set -eu`.
                sh -eu "$script"
                _status=$?
                if [ "$_status" -ne 0 ]; then
                    echo "[Error] $script_name failed (Exit $_status)"
                    printf '%s\n' "$script_name" >> "$SETUP_FAILURES"
                fi
                ;;
            1) ;;
            *)
                echo "[Error] Could not evaluate constraints for $script_name" >&2
                printf '%s\n' "$script_name (constraints)" >> "$SETUP_FAILURES"
                ;;
        esac
    done
done

echo "----------------------------------------------------------------"
if [ -s "$SETUP_FAILURES" ]; then
    echo "[Setup] Failed modules:"
    sed 's/^/  - /' "$SETUP_FAILURES"
    exit 1
fi

echo "[Setup] Completed."
