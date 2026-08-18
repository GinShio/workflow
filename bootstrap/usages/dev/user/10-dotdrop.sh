#!/bin/sh
#@tags: usage:dev, scope:user
# User: compile the dotfiles manifests and deploy them with Dotdrop.

set -eu

# Ensure ~/.local/bin is in PATH for this session (pipx and the wits prefix).
export PATH="$HOME/.local/bin:$PATH"

SETUP_PROFILE="${SETUP_PROFILE:-personal}"
SETUP_HOSTNAME="${SETUP_HOSTNAME:-}"

DOTFILES_ROOT_PATH="${DOTFILES_ROOT_PATH:-$PROJECTS_SCRIPT_DIR/../dotfiles}"
if [ ! -f "$DOTFILES_ROOT_PATH/dotfiles.toml" ]; then
    echo "Error: Missing dotfiles repository at ${DOTFILES_ROOT_PATH} (no dotfiles.toml)"
    exit 1
fi
DOTFILES_ROOT_PATH=$(cd "$DOTFILES_ROOT_PATH" && pwd)

if ! command -v wits >/dev/null 2>&1; then
    echo "Error: wits not found in PATH ($PATH). Did the apps phase install it?"
    exit 1
fi
if ! command -v dotdrop >/dev/null 2>&1; then
    echo "Error: dotdrop not found in PATH ($PATH). Did the apps phase install it?"
    exit 1
fi

WITS_BIN=$(command -v wits)
DOTDROP_BIN=$(command -v dotdrop)

TMPDIR_DOT=$(mktemp -d)
trap 'rm -rf "$TMPDIR_DOT"' EXIT

CONTEXTS_FILE="$TMPDIR_DOT/contexts"
CATALOG_FILE="$TMPDIR_DOT/catalog"
: > "$CONTEXTS_FILE"

lower() {
    printf '%s\n' "$1" | tr '[:upper:]' '[:lower:]'
}

upper() {
    printf '%s\n' "$1" | tr '[:lower:]' '[:upper:]'
}

# Copy TRANSCRYPT_<CTX>_<KEY> onto WITS_TRANSCRYPT_<CTX>_<KEY> when the latter
# is unset, so the documented legacy names still feed the git filter.
map_legacy_transcrypt() {
    _env="$TMPDIR_DOT/env"
    env > "$_env"
    while IFS= read -r _line || [ -n "$_line" ]; do
        _name=${_line%%=*}
        _val=${_line#*=}
        case "$_name" in
            TRANSCRYPT_*_PASSWORD|TRANSCRYPT_*_CIPHER|TRANSCRYPT_*_DIGEST|TRANSCRYPT_*_KDF|TRANSCRYPT_*_ITERATIONS)
                _wits="WITS_TRANSCRYPT_${_name#TRANSCRYPT_}"
                if [ -z "$(printenv "$_wits" 2>/dev/null || true)" ] && [ -n "$_val" ]; then
                    export "$_wits=$_val"
                fi
                ;;
        esac
    done < "$_env"
}

add_context() {
    [ -n "$1" ] || return 0
    if grep -qxF "$1" "$CONTEXTS_FILE" 2>/dev/null; then
        return 0
    fi
    printf '%s\n' "$1" >> "$CONTEXTS_FILE"
}

context_password() {
    printenv "WITS_TRANSCRYPT_$(upper "$1")_PASSWORD" 2>/dev/null || true
}

# Discover every overlay we have a key for. generate compiles every host, so a
# locked fragment on another machine's overlay fails the run — decrypt them all.
collect_contexts() {
    add_context "$SETUP_PROFILE"
    _env="$TMPDIR_DOT/env"
    env > "$_env"
    while IFS= read -r _line || [ -n "$_line" ]; do
        _name=${_line%%=*}
        case "$_name" in
            WITS_TRANSCRYPT_*_PASSWORD)
                _rest=${_name#WITS_TRANSCRYPT_}
                add_context "$(lower "${_rest%_PASSWORD}")"
                ;;
        esac
    done < "$_env"
}

# Point git at wits transcrypt for one overlay, then re-smudge every path that
# overlay's filter owns. Secrets now live in per-module trees, not secret/<profile>.
configure_transcrypt() {
    _ctx="$1"
    _pw=$(context_password "$_ctx")
    if [ -z "$_pw" ]; then
        echo "Info: No credentials for overlay '$_ctx'; leaving those files encrypted."
        return 0
    fi

    _filter="transcrypt-${_ctx}"
    echo "Info: Configuring encryption for overlay '$_ctx'..."

    git config --local -- "filter.${_filter}.clean" \
        "$WITS_BIN transcrypt -C ${_ctx} clean %f"
    git config --local -- "filter.${_filter}.smudge" \
        "$WITS_BIN transcrypt -C ${_ctx} smudge %f"
    git config --local -- "filter.${_filter}.required" true
    git config --local -- "diff.${_filter}.textconv" \
        "$WITS_BIN transcrypt -C ${_ctx} textconv"
    git config --local -- "wits.transcrypt.${_ctx}.password" "$_pw"
    for _key in cipher digest kdf iterations; do
        _val=$(printenv "WITS_TRANSCRYPT_$(upper "$_ctx")_$(upper "$_key")" 2>/dev/null || true)
        if [ -n "$_val" ]; then
            git config --local -- "wits.transcrypt.${_ctx}.${_key}" "$_val"
        fi
    done

    _attrs="$TMPDIR_DOT/attrs.${_ctx}"
    _paths="$TMPDIR_DOT/paths.${_ctx}"
    git ls-files | git check-attr --stdin filter > "$_attrs"
    : > "$_paths"
    while IFS= read -r _line || [ -n "$_line" ]; do
        case "$_line" in
            *": filter: ${_filter}")
                printf '%s\n' "${_line%: filter: ${_filter}}" >> "$_paths"
                ;;
        esac
    done < "$_attrs"

    if [ -s "$_paths" ]; then
        while IFS= read -r _f || [ -n "$_f" ]; do
            [ -n "$_f" ] || continue
            rm -f -- "$_f"
        done < "$_paths"
        git checkout --pathspec-from-file="$_paths"
    fi
}

# `wits dotfiles check` prints one `plane host N unit(s) path` line per
# entrypoint. Host matching: SETUP_HOSTNAME, then the live hostname, then the
# unique host whose overlays include SETUP_PROFILE.
resolve_host() {
    _want=$(lower "$SETUP_HOSTNAME")
    _live=$(lower "$(hostname 2>/dev/null || true)")
    _exact=""
    _case=""
    _live_case=""

    while read -r _plane _host _count _label _path; do
        [ "$_label" = "unit(s)" ] || continue
        [ "$_plane" = "user" ] || continue
        _h=$(lower "$_host")
        if [ "$_host" = "$SETUP_HOSTNAME" ]; then
            _exact="$_host"
        fi
        if [ "$_h" = "$_want" ]; then
            _case="$_host"
        fi
        if [ "$_h" = "$_live" ]; then
            _live_case="$_host"
        fi
    done < "$CATALOG_FILE"

    if [ -n "$_exact" ]; then
        printf '%s\n' "$_exact"
        return 0
    fi
    if [ -n "$_case" ]; then
        echo "Info: Using host '$_case' (case-insensitive match for '$SETUP_HOSTNAME')." >&2
        printf '%s\n' "$_case"
        return 0
    fi
    if [ -n "$_live_case" ]; then
        echo "Info: Using host '$_live_case' (matches live hostname)." >&2
        printf '%s\n' "$_live_case"
        return 0
    fi

    _overlay_hits=""
    while read -r _plane _host _count _label _path; do
        [ "$_label" = "unit(s)" ] || continue
        [ "$_plane" = "user" ] || continue
        _cfg="$DOTFILES_ROOT_PATH/$_path"
        [ -f "$_cfg" ] || continue
        _overlays=$(grep -E '^overlays = ' "$_cfg" || true)
        case "$_overlays" in
            *"\"$SETUP_PROFILE\""*|*"'$SETUP_PROFILE'"*)
                _overlay_hits="$_overlay_hits $_host"
                ;;
        esac
    done < "$CATALOG_FILE"

    _n=0
    _picked=""
    for _h in $_overlay_hits; do
        _n=$((_n + 1))
        _picked=$_h
    done
    if [ "$_n" -eq 1 ]; then
        echo "Info: hostname '$SETUP_HOSTNAME' is not a declared host; using '$_picked' (overlay $SETUP_PROFILE)." >&2
        printf '%s\n' "$_picked"
        return 0
    fi

    echo "Error: No Dotdrop host matches hostname '$SETUP_HOSTNAME' (profile '$SETUP_PROFILE')." >&2
    echo "Declared hosts:" >&2
    while read -r _plane _host _count _label _path; do
        [ "$_label" = "unit(s)" ] || continue
        [ "$_plane" = "user" ] || continue
        echo "  $_host" >&2
    done < "$CATALOG_FILE"
    echo "Pass --hostname with one of the names above." >&2
    return 1
}

install_plane() {
    _plane="$1"
    _host="$2"
    _path=""
    while read -r _p _h _count _label _cfg; do
        [ "$_label" = "unit(s)" ] || continue
        if [ "$_p" = "$_plane" ] && [ "$_h" = "$_host" ]; then
            _path="$_cfg"
            break
        fi
    done < "$CATALOG_FILE"

    if [ -z "$_path" ]; then
        echo "Info: Host '$_host' has no '$_plane' plane; skipping."
        return 0
    fi

    _cfg="$DOTFILES_ROOT_PATH/$_path"
    if [ ! -f "$_cfg" ]; then
        echo "Error: Missing generated config $_cfg"
        return 1
    fi

    echo "Installing $_plane plane from $_path (profile $_host)..."
    if [ "$_plane" = "system" ]; then
        sudo -AE env "HOME=$HOME" "PATH=$PATH" \
            "$DOTDROP_BIN" install -f -c "$_cfg" -p "$_host"
    else
        "$DOTDROP_BIN" install -f -c "$_cfg" -p "$_host"
    fi
}

map_legacy_transcrypt
collect_contexts

(
    cd "$DOTFILES_ROOT_PATH"
    if [ -s "$CONTEXTS_FILE" ]; then
        while IFS= read -r _ctx || [ -n "$_ctx" ]; do
            [ -n "$_ctx" ] || continue
            configure_transcrypt "$_ctx"
        done < "$CONTEXTS_FILE"
    fi
)

echo "Generating Dotdrop configs..."
"$WITS_BIN" dotfiles generate --root "$DOTFILES_ROOT_PATH"
"$WITS_BIN" dotfiles check --root "$DOTFILES_ROOT_PATH" > "$CATALOG_FILE"
cat "$CATALOG_FILE"

HOST=$(resolve_host)
echo "Info: Deploying host '$HOST' (overlay '$SETUP_PROFILE')."

(
    cd "$DOTFILES_ROOT_PATH"
    install_plane user "$HOST"
    install_plane system "$HOST"
)
