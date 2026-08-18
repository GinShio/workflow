#!/bin/sh
#
# ssh-key.sh - Key Generator & Deployment tool
#
# This script manages SSH and GPG keys, supporting generation and deployment.
# It is designed to be POSIX compliant, cross-platform, and semantic.
#
# Usage:
#   ./ssh-key.sh [command] [options]
#
# Author: GitHub Copilot (Refactored for User)
# SPDX-License-Identifier: MIT

set -e

# ==============================================================================
# Configuration & Defaults
# ==============================================================================

ENV_FILE="$XDG_CONFIG_HOME/workflow/.env"
TIMESTAMP=$(date "+%Y")
ARCHIVE_NAME="ssh-${TIMESTAMP}"
TMP_TEMPLATE="/tmp/key-gen-XXXXXXXX"

# ANSI Colors
C_RESET='\033[0m'
C_RED='\033[31m'
C_GREEN='\033[32m'
C_YELLOW='\033[33m'
C_BLUE='\033[34m'

# ==============================================================================
# Helper Functions
# ==============================================================================

log_header() { printf "${C_BLUE}==>${C_RESET} %s\n" "$*"; }
log_info()   { printf "${C_GREEN}[INFO]${C_RESET} %s\n" "$*"; }
log_warn()   { printf "${C_YELLOW}[WARN]${C_RESET} %s\n" "$*"; }
log_error()  { printf "${C_RED}[ERROR]${C_RESET} %s\n" "$*"; }

fail() {
    log_error "$*"
    exit 1
}

# Run-or-echo helper for dry-run mode
run_or_echo() {
    # Usage: run_or_echo CMD ARGS...
    if [ "${DRY_RUN:-0}" = "1" ]; then
        log_info "[DRY] $*"
        return 0
    fi
    "$@"
}

# Centralized cleanup for temporary resources. Registered variables:
# - TEMP_DIR: used during generate
# - tmp_extract: used during deploy
# This function is safe to call multiple times.
cleanup() {
    # Centralized cleanup
    for var_name in TEMP_DIR tmp_extract; do
        # POSIX compliant indirect reference
        eval "path_to_remove=\${$var_name}"

        if [ -n "$path_to_remove" ] && [ -d "$path_to_remove" ]; then
            # Basic sanity check to prevent deleting unexpected paths
            # Ensure it is an absolute path (starts with /)
            case "$path_to_remove" in
                /*)
                    rm -rf "$path_to_remove" 2>/dev/null || true
                    ;;
                *)
                    log_warn "Skipping cleanup of unsafe path: $path_to_remove"
                    ;;
            esac
        fi
    done
}

# Ensure cleanup runs on exit and signals
trap 'cleanup' EXIT INT TERM HUP

check_deps() {
    missing=0
    for cmd in "$@"; do
        [ -z "$cmd" ] && continue

        # Preferred check
        if command -v "$cmd" >/dev/null 2>&1; then
            continue
        fi

        # Fallback: some shells may behave differently; try 'type'
        if type "$cmd" >/dev/null 2>&1 2>/dev/null; then
            continue
        fi

        # Final fallback: use which if available
        if command -v which >/dev/null 2>&1 && which "$cmd" >/dev/null 2>&1; then
            continue
        fi

        log_error "Missing required command: $cmd"
        missing=1
    done

    if [ "$missing" -eq 1 ]; then
        return 1
    fi
}

normalize_path() {
    # Simple normalization for display/usage
    echo "$1" | sed "s|$HOME|~|"
}

load_env() {
    if [ -f "$ENV_FILE" ]; then
        log_info "Loading environment from $(normalize_path "$ENV_FILE")"
        set -a
        # shellcheck disable=SC1090,SC1091
        . "$ENV_FILE"
        set +a
    else
        log_warn "Environment file not found at $ENV_FILE"
    fi
}

# ==============================================================================
# Functional Logic
# ==============================================================================

# --- SSH Generation ---

generate_ssh_key() {
    key_path="$1"
    email="$2"
    comment="$3"

    if [ -f "$key_path" ]; then
        log_warn "SSH key already exists: $(normalize_path "$key_path") (Skipping)"
        return
    fi

    log_info "Generating SSH key: $key_path ($comment)"
    if [ "${DRY_RUN:-0}" = "1" ]; then
        log_info "[DRY] ssh-keygen -C '$comment' -t ed25519 -f '$key_path' -N ''"
        return
    fi
    ssh-keygen -C "$comment" -t ed25519 -f "$key_path" -N "" >/dev/null 2>&1
}

process_ssh_generation() {
    work_dir="$1"
    cd "$work_dir" || fail "Failed to enter directory: $work_dir"

    log_header "Starting SSH Key Generation..."

    # Extract all environment variables ending in _EMAIL
    # Since we are strict POSIX, we use env and text processing
    env_vars=$(env | cut -d= -f1 | grep '_EMAIL$' 2>/dev/null || true)

    if [ -z "$(echo "$env_vars" | tr -d ' \n\t')" ]; then
        log_warn "No identities found: no environment variables ending with _EMAIL"
    fi

    for email_var in $env_vars; do
        # Extract Prefix (e.g. PERSONAL_EMAIL -> PERSONAL)
        prefix=${email_var%_EMAIL}

        # Get value of the variable slightly indirectly for POSIX
        eval "email=\${$email_var}"

        if [ -z "$email" ]; then
            continue
        fi

        # Convert to lowercase
        prefix_lower=$(echo "$prefix" | tr '[:upper:]' '[:lower:]')

        log_info "Processing Identity: $prefix ($email)"

        # Define key naming strategy
        # If PERSONAL, use specific legacy names if desired, or standardized ones
        if [ "$prefix" = "PERSONAL" ]; then
             generate_ssh_key "personal-ssh" "$email" "personal-ssh-$email"
             generate_ssh_key "personal-git" "$email" "personal-git-$email"
        else
            # Standard pattern for others
            generate_ssh_key "${prefix_lower}-pri-ssh" "$email" "${prefix_lower}-pri-ssh-$email"
            generate_ssh_key "${prefix_lower}-pri-git" "$email" "${prefix_lower}-pri-git-$email"
            generate_ssh_key "${prefix_lower}-pub-git" "$email" "${prefix_lower}-pub-git-$email"
        fi
    done

    # Secure keys (only if files exist)
    for f in ./*; do
        [ -e "$f" ] || continue
        chmod 600 "$f" 2>/dev/null || true
    done
}

# --- GPG Generation ---

generate_gpg_key() {
    work_dir="$1"
    cd "$work_dir" || fail "Failed to enter directory: $work_dir"

    log_header "Starting GPG Key Generation..."

    env_vars=$(env | cut -d= -f1 | grep '_EMAIL$' 2>/dev/null || true)

    for email_var in $env_vars; do
        prefix=${email_var%_EMAIL}
        eval "email=\${$email_var}"
        eval "name=\${${prefix}_NAME}" # Look for corresponding _NAME var

        # Default name if not provided
        name="${name:-$prefix User}"

        if [ -z "$email" ]; then continue; fi

        prefix_lower=$(echo "$prefix" | tr '[:upper:]' '[:lower:]')
        key_file="${prefix_lower}-gpg.asc"

        log_info "Generating GPG Key for $email ($name)..."

        if [ "${DRY_RUN:-0}" = "1" ]; then
            log_info "[DRY] cat > gpg-batch <<'EOF'"
            log_info "%echo Generating a basic OpenPGP key"
            log_info "Key-Type: EDDSA"
            log_info "Key-Curve: ed25519"
            log_info "Key-Usage: sign"
            log_info "Subkey-Type: ECDH"
            log_info "Subkey-Curve: cv25519"
            log_info "Subkey-Usage: encrypt"
            log_info "Name-Real: $name"
            log_info "Name-Email: $email"
            log_info "Expire-Date: 0"
            log_info "%no-protection"
            log_info "%commit"
            log_info "%echo done"
            log_info "EOF"
            log_info "[DRY] gpg --batch --generate-key gpg-batch"
            log_info "[DRY] gpg --armor --export '$email' > '${key_file}'"
            log_info "[DRY] gpg --armor --export-secret-keys '$email' > '${key_file}.secret'"
            continue
        fi

        # Create batch config for GPG
        cat > gpg-batch <<EOF
%echo Generating a basic OpenPGP key
Key-Type: EDDSA
Key-Curve: ed25519
Key-Usage: sign
Subkey-Type: ECDH
Subkey-Curve: cv25519
Subkey-Usage: encrypt
Name-Real: $name
Name-Email: $email
Expire-Date: 0
%no-protection
%commit
%echo done
EOF

        # Run GPG in batch mode, setting GNUPGHOME to temp dir to avoid messing with user's keyring
        export GNUPGHOME="$work_dir/gnupg"
        mkdir -p "$GNUPGHOME"
        chmod 700 "$GNUPGHOME"

        if gpg --batch --generate-key gpg-batch >/dev/null 2>&1; then
             # Export the key
             gpg --armor --export "$email" > "$key_file"
             gpg --armor --export-secret-keys "$email" > "${key_file}.secret"
             log_info "Generated GPG key pair saved to $key_file"
        else
             log_error "Failed to generate GPG key for $email"
        fi

        rm gpg-batch
        # GNUPGHOME lives under the temporary work_dir and will be cleaned up by cleanup()
    done
}


# --- Packaging & Archives ---

archive_keys() {
    work_dir="$1"
    output_dir="${DOTFILES_ROOT_DIR:-$HOME}/keys"

    if [ ! -d "$output_dir" ]; then
        log_warn "Destination directory $output_dir does not exist. Creating..."
        mkdir -p "$output_dir"
    fi

    log_header "Archiving keys..."
    cd "$work_dir"

    archive_file="${ARCHIVE_NAME}.tar.zst"

    if [ "${DRY_RUN:-0}" = "1" ]; then
        log_info "[DRY] tar --exclude='gnupg' -cf - . | zstd -z -19 --ultra --quiet -o \"$archive_file\""
        log_info "[DRY] rsync --remove-source-files \"$archive_file\" \"$output_dir/\""
    else
        # Exclude GPG temp dir if it remains
        tar --exclude='gnupg' -cf - . | zstd -z -19 --ultra --quiet -o "$archive_file"

        log_info "Created archive: $archive_file"

        rsync --remove-source-files "$archive_file" "$output_dir/"
        log_info "Moved archive to $output_dir/"
    fi

    # Link
    if [ "${DRY_RUN:-0}" = "1" ]; then
        log_info "[DRY] cd \"$output_dir\" && ln -sf \"$archive_file\" ssh.tar.zst"
    else
        cd "$output_dir"
        ln -sf "$archive_file" ssh.tar.zst
        log_info "Updated symlink ssh.tar.zst -> $archive_file"
    fi
}

# --- Deploy ---

deploy_keys() {
    dest_dir="$HOME/.ssh"
    src_archive="${DOTFILES_ROOT_DIR:-$HOME}/keys/ssh.tar.zst"
    tmp_extract=$(mktemp -d "$TMP_TEMPLATE")

    if [ ! -f "$src_archive" ]; then
        if [ "${DRY_RUN:-0}" = "1" ]; then
            log_info "[DRY] Archive not found: $src_archive (would abort in real run)"
        else
            fail "Archive not found: $src_archive"
        fi
    fi

    log_header "Deploying keys from $src_archive"

    if ! command -v zstd >/dev/null; then
        fail "zstd is required for decompression"
    fi
    if [ "${DRY_RUN:-0}" = "1" ]; then
        log_info "[DRY] mkdir -p \"$tmp_extract\" && zstd -d -c \"$src_archive\" | tar -C \"$tmp_extract\" -xf -"
        log_info "[DRY] rsync -av --remote-option=--chmod=600 \"$tmp_extract/\" \"$dest_dir/\""
        log_info "[DRY] mkdir -p \"$dest_dir\" && chmod 700 \"$dest_dir\""
        log_info "[DRY] chmod 600 \"$dest_dir\"/* && chmod 644 \"$dest_dir\"/*.pub"
        # tmp_extract will be removed by cleanup()
        return
    fi

    # Extract
    log_info "Extracting to temporary buffer..."
    # Decompress using zstd -d and pipe to tar
    zstd -d -c "$src_archive" | tar -C "$tmp_extract" -xf -

    # Allow some cleanup of old archives if they were extracted by mistake
    rm -f "$tmp_extract"/*.tar.zst*

    if [ ! -d "$dest_dir" ]; then
        mkdir -p "$dest_dir"
        chmod 700 "$dest_dir"
    fi

    log_info "Syncing to $dest_dir..."
    rsync -av --remote-option=--chmod=600 "$tmp_extract/" "$dest_dir/"

    # Fix permissions robustly
    chmod 700 "$dest_dir"
    chmod 600 "$dest_dir"/*
    chmod 644 "$dest_dir"/*.pub 2>/dev/null || true

    # tmp_extract will be removed by cleanup()
    log_info "Deployment complete."
}

# ==============================================================================
# Usage & Main
# ==============================================================================

show_usage() {
    cat <<EOF
Usage: $(basename "$0") <command> [options]

Commands:
  gen, generate     Generate SSH and GPG keys based on environment variables.
                    Scans for variables ending in _EMAIL (e.g. WORK_EMAIL).
  deploy            Deploy keys from storage to ~/.ssh.
  help              Show this help message.

Options:
  --ssh-only        Only generate SSH keys (for 'gen' command).
  --gpg-only        Only generate GPG keys (for 'gen' command).
  --dry-run         Show what would be done (dry-run will not perform actions).

Examples:
  $(basename "$0") generate
  $(basename "$0") deploy
EOF
    exit 0
}

# Main Execution

# Parse global options first if needed, but simple handling for now
COMMAND="$1"
shift 1>/dev/null 2>&1 || true

case "$COMMAND" in
    gen|generate)
        # Sub-argument parsing (so DRY_RUN and selectors are known before checks)
        DO_SSH=1
        DO_GPG=1
        DRY_RUN=${DRY_RUN:-0}
        # parse simple sub-arguments
        for arg in "$@"; do
            case "$arg" in
                --ssh-only) DO_GPG=0 ;;
                --gpg-only) DO_SSH=0 ;;
                --dry-run) DRY_RUN=1 ;;
                *) ;;
            esac
        done

        # If dry-run requested, skip fatal dependency checks (they are only needed for actual execution)
        if [ "${DRY_RUN:-0}" != "1" ]; then
            if ! check_deps rsync tar ssh-keygen zstd; then
                fail "Missing required commands. Please install dependencies or adjust PATH."
            fi
        else
            log_info "[DRY] skipping dependency checks"
        fi

        # Load environment (will try override and fallbacks)
        load_env || log_warn "Continuing without environment file; no _EMAIL vars may be present"

        TEMP_DIR=$(mktemp -d "$TMP_TEMPLATE")
        # TEMP_DIR is tracked by cleanup() via trap set at script top

        if [ $DO_SSH -eq 1 ]; then process_ssh_generation "$TEMP_DIR"; fi
        if [ $DO_GPG -eq 1 ]; then
            if command -v gpg >/dev/null; then
                generate_gpg_key "$TEMP_DIR"
            else
                log_warn "gpg not found, skipping GPG generation"
            fi
        fi

        archive_keys "$TEMP_DIR"
        ;;

    deploy)
        # Parse deploy sub-arguments (support --dry-run)
        for arg in "$@"; do
            case "$arg" in
                --dry-run) DRY_RUN=1 ;;
                *) ;;
            esac
        done

        if [ "${DRY_RUN:-0}" != "1" ]; then
            if ! check_deps rsync tar zstd; then
                fail "Missing required commands. Please install dependencies or adjust PATH."
            fi
        else
            log_info "[DRY] skipping dependency checks"
        fi
        load_env
        deploy_keys
        ;;

    help|-h|--help)
        show_usage
        ;;

    *)
        log_error "Unknown command or missing arguments."
        show_usage
        ;;
esac
