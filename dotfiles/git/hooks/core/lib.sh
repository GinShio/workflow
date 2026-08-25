#!/bin/sh

# Core git-hooks library: shared state and utilities sourced by every hook script.

# --- Configuration ---

# Helper to check boolean values.
is_truthy() {
    case "$1" in
        [Yy][Ee][Ss]|[Yy]|[Tt][Rr][Uu][Ee]|1|[Oo][Nn]) return 0 ;;
        *) return 1 ;;
    esac
}

# The environment-variable twin of a config key is a pure mechanical transform:
# upper-case the whole key and turn every `-` and `.` into `_`. So
# `wits.hooks.pre-commit.formatter-disable` maps to
# WITS_HOOKS_PRE_COMMIT_FORMATTER_DISABLE — no prefix juggling, no special case.
_cfg_env_name() {
    echo "$1" | tr '[:lower:]' '[:upper:]' | tr '.-' '__'
}

# True when $1 is a plain shell identifier ([A-Za-z_][A-Za-z0-9_]*). Config keys
# come from a repo's own `.git/config`, whose quoted subsection can hold
# arbitrary bytes; a twin name is trusted (fed to the reads below and to
# `export`) only after passing this gate, so a crafted key can never smuggle
# shell syntax in.
_is_identifier() {
    case "$1" in
        '' | [!A-Za-z_]* | *[!A-Za-z0-9_]*) return 1 ;;
        *) return 0 ;;
    esac
}

# Read the environment variable *named by* $1, without its value ever being
# treated as code. POSIX sh has no `${!name}`, so indirection needs one `eval`;
# we make it provably safe by (a) refusing a non-identifier name and (b) only
# ever *reading* — the value is printed, never re-parsed. This replaces the older
# habit of `eval "$name=$value"`, where a config value could be executed.
_env_get() {
    _is_identifier "$1" || return 1
    eval "printf '%s' \"\${$1-}\""
}

# True when the (identifier) variable named by $1 is set, even if empty. Same
# safety contract as `_env_get`.
_env_is_set() {
    _is_identifier "$1" || return 1
    eval "[ \"\${$1+x}\" = x ]"
}

# Resolve a setting, environment twin first, then git config. This is the single
# path every script uses, so one rule holds everywhere: env overrides config,
# config is the standing value.
#
# The runner batches this hook's config (its own namespace plus the top-level
# globals) into env twins once (see core/runner's warm_config) and sets
# _WITS_CONFIG_WARMED; when that flag is present the twin already reflects config,
# so the per-call `git config` fork is skipped — an unset twin then means "unset,
# use the default". Outside the runner (no warm), the live `git config` fallback
# still runs, so these stay correct anywhere.
#
#   cfg_bool  <config-key> [default]   -> exit status (0 = true)
#   cfg_value <config-key> [default]   -> echoes the resolved string
cfg_bool() {
    _env_name=$(_cfg_env_name "$1")
    if _env_is_set "$_env_name"; then
        _val=$(_env_get "$_env_name")
        is_truthy "$_val"
        return
    fi
    if [ -z "${_WITS_CONFIG_WARMED:-}" ]; then
        _val=$(git config --bool "$1" 2>/dev/null)
        [ -n "$_val" ] && { is_truthy "$_val"; return; }
    fi
    is_truthy "${2:-false}"
}
cfg_value() {
    _env_name=$(_cfg_env_name "$1")
    if _env_is_set "$_env_name"; then
        _env_get "$_env_name"
        printf '\n'
        return
    fi
    if [ -z "${_WITS_CONFIG_WARMED:-}" ]; then
        _val=$(git config "$1" 2>/dev/null)
        [ -n "$_val" ] && { printf '%s\n' "$_val"; return; }
    fi
    printf '%s\n' "${2:-}"
}


# Colors
if [ -t 1 ]; then
    COLOR_RED=$(printf '\033[0;31m')
    COLOR_GREEN=$(printf '\033[0;32m')
    COLOR_YELLOW=$(printf '\033[0;33m')
    COLOR_CYAN=$(printf '\033[0;36m')
    COLOR_RESET=$(printf '\033[0m')
else
    COLOR_RED=""
    COLOR_GREEN=""
    COLOR_YELLOW=""
    COLOR_CYAN=""
    COLOR_RESET=""
fi

# A literal newline, for building and matching newline-delimited lists in the
# portable subset (no arrays, no `read -d`).
LF='
'

# Logging levels: 0=OFF, 1=ERROR, 2=WARN, 3=INFO, 4=DEBUG (default WARN).
# Configured via ENV: WITS_HOOKS_LOG_LEVEL
log_level=${WITS_HOOKS_LOG_LEVEL:-2}

# Enable shell tracing for debug level
if [ "$log_level" -ge 4 ]; then
    set -x
fi

# All diagnostics go to stderr: a hook's stdout can be meaningful (or piped),
# and by convention progress/errors belong on fd 2 so they stay visible even
# when stdout is captured or redirected.
log_debug() {
    if [ "$log_level" -ge 4 ]; then
        printf "%s[DEBUG]%s %s\n" "$COLOR_CYAN" "$COLOR_RESET" "$*" >&2
    fi
}
log_info() {
    if [ "$log_level" -ge 3 ]; then
        printf "%s[INFO]%s %s\n" "$COLOR_GREEN" "$COLOR_RESET" "$*" >&2
    fi
}
log_warn() {
    if [ "$log_level" -ge 2 ]; then
        printf "%s[WARN]%s %s\n" "$COLOR_YELLOW" "$COLOR_RESET" "$*" >&2
    fi
}
log_error() {
    if [ "$log_level" -ge 1 ]; then
        printf "%s[ERROR]%s %s\n" "$COLOR_RED" "$COLOR_RESET" "$*" >&2
    fi
}

# --- Common utilities ---

prompt_confirm() {
    _msg="${1:-Are you sure want to continue? [y/N] }"
    # Read the answer straight from the controlling terminal for this one prompt,
    # rather than `exec < /dev/tty`, which would permanently reassign fd 0 and
    # swallow whatever the hook is still streaming on stdin (e.g. the pre-push
    # ref list the caller loops over). No terminal (CI, no tty) means we cannot
    # ask, so decline safely.
    [ -r /dev/tty ] || return 1
    printf "%s%s%s " "$COLOR_YELLOW" "$_msg" "$COLOR_RESET" >&2
    read -r _response < /dev/tty || return 1
    case "$_response" in
        [yY][eE][sS]|[yY]) return 0 ;;
        *) return 1 ;;
    esac
}

# Resolve build directories for a specific branch using wits.
# Usage: resolve_build_dirs <branch_name>
resolve_build_dirs() {
    _branch="$1"
    _repo="$GIT_TOPLEVEL"
    _bd=""

    command -v wits >/dev/null 2>&1 || {
        log_warn "cleanup-build-dir: wits is unavailable; no build directory will be deleted."
        return 1
    }
    _bd=$(wits project build-dir "$_repo" --branch "$_branch" 2>/dev/null) || {
        log_warn "cleanup-build-dir: cannot resolve build directory for $_branch."
        return 1
    }
    [ -n "$_bd" ] || return 1

    _main_branch=$(wits project main-branch "$_repo" 2>/dev/null) || return 1
    _main_bd=$(wits project build-dir "$_repo" --branch "$_main_branch" 2>/dev/null) ||
        return 1

    # A branch cleanup may remove variants of that branch's base, but never a
    # build root shared with the main branch.
    _bd_base=${_bd%-debug}
    _bd_base=${_bd_base%-release}
    _main_base=${_main_bd%-debug}
    _main_base=${_main_base%-release}
    if [ "$_bd_base" = "$_main_base" ]; then
        log_warn "cleanup-build-dir: $_branch shares $_bd_base with $_main_branch; keeping it."
        return 1
    fi
    _build_parent=$(dirname "$_main_base")
    _build_root=$(CDPATH= cd -P "$_build_parent" 2>/dev/null && pwd) || {
        log_warn "cleanup-build-dir: cannot prove build root $_build_parent."
        return 1
    }
    [ "$_build_root" != / ] || {
        log_warn "cleanup-build-dir: refusing filesystem root as a build root."
        return 1
    }

    for suffix in "" "-debug" "-release"; do
        _bd_target="${_bd_base}${suffix}"
        [ -d "$_bd_target" ] || continue
        if [ -L "$_bd_target" ]; then
            log_warn "cleanup-build-dir: refusing symlink $_bd_target."
            continue
        fi
        _bd_safe=$(CDPATH= cd -P "$_bd_target" 2>/dev/null && pwd) || continue
        case "$_bd_safe" in
            /|"$HOME"|"$GIT_TOPLEVEL")
                log_warn "cleanup-build-dir: refusing unsafe path $_bd_safe."
                continue
                ;;
        esac
        if [ "$_bd_safe" = "$_build_root" ]; then
            log_warn "cleanup-build-dir: refusing build root $_bd_safe."
            continue
        fi
        case "$_bd_safe/" in
            "$_build_root/"*) ;;
            *)
                log_warn "cleanup-build-dir: $_bd_safe is outside $_build_root."
                continue
                ;;
        esac
        case "$GIT_TOPLEVEL/" in
            "$_bd_safe/"*)
                log_warn "cleanup-build-dir: refusing repository ancestor $_bd_safe."
                continue
                ;;
        esac
        printf '%s\n' "$_bd_safe"
    done
}

# Resolve Main/Default Branch Name
# Usage: get_main_branch [remote_name]
get_main_branch() {
    _remote="${1:-origin}"

    # 1. The wits project registry — authoritative for a known project, below an
    # explicit git-config override but above the remote-HEAD / name guesses.
    if command -v wits >/dev/null 2>&1; then
        _wits_mb=$(wits project main-branch 2>/dev/null) &&
            [ -n "$_wits_mb" ] && { echo "$_wits_mb"; return; }
    fi

    # 2. Check local tracking info (fastest)
    if _remote_head=$(git symbolic-ref "refs/remotes/$_remote/HEAD" 2>/dev/null); then
        echo "${_remote_head#refs/remotes/$_remote/}"
        return
    fi

    # 2.1 Verify if 'refs/remotes/origin/HEAD' is missing, try to detect it once?
    # This invokes network and is slow, so we only implicitly trust if cached.
    # Alternatively, users should run `git remote set-head origin -a`

    # 3. Guess common names
    for _candidate in main master trunk development; do
        if git show-ref --verify --quiet "refs/heads/$_candidate"; then
            echo "$_candidate"
            return
        fi
        if git show-ref --verify --quiet "refs/remotes/$_remote/$_candidate"; then
            echo "$_candidate"
            return
        fi
    done

    # 4. Fallback
    echo "master"
}

# --- Staged content ---
#
# A pre-commit hook judges what is *being committed* — the staged blob — not the
# working tree, which may carry unstaged edits. These helpers let every script
# speak in terms of the index consistently.

# The staged paths a pre-commit script cares about: added, copied, or modified.
# Served from the pre-commit cache when present (resolved once in the state
# block above), otherwise a live query so the helper still works in any hook.
staged_files() {
    if [ -n "${_WITS_STAGED_CACHED:-}" ]; then
        [ -n "$STAGED_FILES" ] && printf '%s\n' "$STAGED_FILES"
        return 0
    fi
    git diff --cached --name-only --diff-filter=ACM
}

# The staged content of a file, straight from the index.
staged_blob() {
    git cat-file blob ":$1" 2>/dev/null
}

# Size of the staged blob, in bytes.
staged_size() {
    git cat-file -s ":$1" 2>/dev/null
}

is_staged_regular() {
    if [ -n "${_WITS_STAGED_CACHED:-}" ]; then
        case "$LF$STAGED_REGULAR_FILES$LF" in
            *"$LF$1$LF"*) return 0 ;;
            *) return 1 ;;
        esac
    fi
    _regular_mode=$(git ls-files --stage -- "$1" | cut -d' ' -f1)
    case "$_regular_mode" in
        100???) return 0 ;;
        *) return 1 ;;
    esac
}

# True when the staged blob is text (git's own heuristic: a diff against a
# binary blob reports '-' additions instead of a line count). When the
# pre-commit cache is populated this is a membership test against the precomputed
# text set (no fork); otherwise it falls back to a live per-file query.
is_staged_text() {
    is_staged_regular "$1" || return 1
    if [ -n "${_WITS_STAGED_CACHED:-}" ]; then
        case "$LF$STAGED_TEXT_FILES$LF" in
            *"$LF$1$LF"*) return 0 ;;
            *) return 1 ;;
        esac
    fi
    [ "$(git diff --cached --numstat -- "$1" | cut -f1)" != "-" ]
}

# True when a filter name is an encrypting clean/smudge filter (transcrypt,
# git-crypt). A predicate over the *name* rather than a path, so a batch
# `check-attr` scan over the whole tree and the per-file `is_encrypted` share
# one definition of what counts as encrypted.
is_crypt_filter() {
    case "$1" in
        transcrypt|transcrypt-*|git-crypt|git-crypt-*|crypt|crypt-*) return 0 ;;
        *) return 1 ;;
    esac
}

# True when a file is managed by an encrypting clean/smudge filter (transcrypt,
# git-crypt): its staged blob is ciphertext, not content we should format or
# inspect, so content hooks skip it.
is_encrypted() {
    _encrypted_attr=$(git check-attr filter -- "$1" 2>/dev/null)
    _encrypted_filter=${_encrypted_attr##*: filter: }
    is_crypt_filter "$_encrypted_filter"
}

# Echo the staged text paths whose extension matches one of the given suffixes,
# skipping binary and encrypted blobs. This is the one line every per-language
# formatter/linter shares, so a new language is just a new one-concern script
# that calls this with its extensions. Usage: staged_lang_files .py .pyi
staged_lang_files() {
    staged_files | while IFS= read -r _slf; do
        is_staged_text "$_slf" || continue
        is_encrypted "$_slf" && continue
        for _ext in "$@"; do
            case "$_slf" in
                *"$_ext") printf '%s\n' "$_slf"; break ;;
            esac
        done
    done
}

# True when the working tree differs from the index for a file — i.e. it is only
# partially staged, so rewriting the whole file would capture unstaged edits.
has_unstaged_changes() {
    ! git diff --quiet -- "$1"
}

# Format a file's *staged content* in place in the index, leaving unstaged edits
# untouched. The command reads the blob on stdin and writes the result to
# stdout; if it changes anything, the new content is written back to the index,
# and to the working tree too when that is safe (no unstaged edits to clobber).
# Usage: apply_to_staged <file> <formatter> [args...]
apply_to_staged() {
    _f="$1"
    shift
    _in=$(mktemp) || return 1
    _out=$(mktemp) || { rm -f "$_in"; return 1; }
    _err=$(mktemp) || { rm -f "$_in" "$_out"; return 1; }
    _apply_status=0

    if ! staged_blob "$_f" > "$_in"; then
        log_error "Could not read staged content for $_f"
        _apply_status=1
    elif ! "$@" < "$_in" > "$_out" 2>"$_err"; then
        log_error "Formatter failed for staged file $_f"
        # A non-zero return here aborts the whole hook, and so the commit; the
        # tool's own diagnostic is the only thing that says why. Warnings from
        # a run that succeeded stay hidden.
        if [ -s "$_err" ]; then
            cat "$_err" >&2
        fi
        _apply_status=1
    else
        cmp -s "$_in" "$_out"
        _cmp_status=$?
        case "$_cmp_status" in
            0) ;;
            1)
                # Decide before rewriting the index: afterwards the old working
                # copy would always differ from the freshly formatted index.
                _sync_worktree=1
                has_unstaged_changes "$_f" && _sync_worktree=0

                _mode=$(git ls-files --stage -- "$_f" | cut -d' ' -f1) ||
                    _apply_status=1
                if [ "$_apply_status" -eq 0 ]; then
                    _sha=$(git hash-object -w "$_out") || _apply_status=1
                fi
                if [ "$_apply_status" -eq 0 ]; then
                    git update-index --cacheinfo "$_mode" "$_sha" "$_f" ||
                        _apply_status=1
                fi
                if [ "$_apply_status" -eq 0 ] && [ "$_sync_worktree" -eq 1 ]; then
                    git checkout-index -f -- "$_f" || _apply_status=1
                fi
                ;;
            *)
                log_error "Could not compare formatter output for $_f"
                _apply_status=1
                ;;
        esac
    fi

    rm -f "$_in" "$_out" "$_err"
    return "$_apply_status"
}
