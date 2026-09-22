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

# --- Repo facts ---
#
# Each resolver memoizes into (and exports) its well-known variable, so the cost
# is paid once per process tree and every later reader sees a plain $VAR. They
# honour a value already in the environment — one git exported, or one a parent
# already resolved — which is what lets core/runner pre-resolve a hook's facts
# for every child, and lets a child resolve one the runner did not.
#
# They live here rather than in the runner precisely so a `.d` script can call
# the one it needs *after* its early-exit guards. On the hot hooks that is the
# difference between forking git on every ref update and forking it only on the
# rare update a script actually acts on.
git_dir() {
    [ -n "${GIT_DIR:-}" ] || { GIT_DIR=$(git rev-parse --git-dir); export GIT_DIR; }
}
git_common_dir() {
    [ -n "${GIT_COMMON_DIR:-}" ] ||
        { GIT_COMMON_DIR=$(git rev-parse --git-common-dir); export GIT_COMMON_DIR; }
}
git_toplevel() {
    if [ -z "${GIT_TOPLEVEL:-}" ]; then
        GIT_TOPLEVEL=$(git rev-parse --show-toplevel 2>/dev/null)
        # A bare repository has no working tree; fall back to the git dir.
        [ -n "$GIT_TOPLEVEL" ] || { git_dir; GIT_TOPLEVEL=$GIT_DIR; }
        export GIT_TOPLEVEL
    fi
}
current_branch() {
    [ -n "${CURRENT_BRANCH:-}" ] ||
        { CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null); export CURRENT_BRANCH; }
}
null_sha() {
    [ -n "${NULL_SHA:-}" ] ||
        { NULL_SHA=$(git hash-object --stdin </dev/null | tr '0-9a-f' '0'); export NULL_SHA; }
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

# The build directory whose compile_commands.json this checkout should point at.
# Usage: active_build_dir <branch_name>
#
# One line, or nothing when the project declares no build_dir. Which project
# answers is wits's decision: for a checkout several projects share, it follows
# `wits.project.active`, so a component borrowed by the project you actually
# build resolves to that project's tree instead of its owner's.
#
# The directory is deliberately *not* required to exist. A checkout happens
# before the branch has ever been built, so demanding it would make the common
# case a no-op — and leave the previous branch's link in place, which is a stale
# index silently describing another branch's build.
active_build_dir() {
    _abd_branch="$1"
    command -v wits >/dev/null 2>&1 || {
        log_warn "build-dir: wits is unavailable; leaving compile_commands.json alone."
        return 1
    }
    # Resolved here rather than assumed: a caller whose hook does not warm the
    # fact would otherwise pass an empty path, and wits would answer for the
    # wrong repository (or none) instead of failing. Memoized, so free when the
    # runner already resolved it.
    git_toplevel
    wits project info --get build_dir "$GIT_TOPLEVEL" --branch "$_abd_branch" 2>/dev/null
}

# Every build directory that a branch of this checkout identifies, one per line,
# filtered to what is safe to delete.
# Usage: branch_build_dirs <branch_name>
#
# The answer spans projects, and that is the point: a borrowed component can be
# the build identity of several projects at once, so a single-project answer
# leaves every borrower's tree behind to accumulate forever. wits decides which
# projects qualify and which build types exist; this adds only the guards that
# belong beside an `rm -rf`.
branch_build_dirs() {
    _bbd_branch="$1"
    command -v wits >/dev/null 2>&1 || {
        log_warn "build-dirs: wits is unavailable; no build directory will be deleted."
        return 1
    }
    git_toplevel   # see active_build_dir: never assume the caller warmed it
    _bbd_tab=$(printf '\t')
    wits project branch-build-dirs "$GIT_TOPLEVEL" --branch "$_bbd_branch" 2>/dev/null |
        while IFS="$_bbd_tab" read -r _bbd_project _bbd_dir; do
            [ -n "$_bbd_dir" ] || continue
            removable_build_dir "$_bbd_dir" || continue
            printf '%s\n' "$_bbd_dir"
        done
}

# Whether a path may be handed to `rm -rf` as a build directory.
# Usage: removable_build_dir <path>
#
# wits only ever names a directory its own registry resolved, so nothing here is
# expected to fire. They stay because the caller deletes recursively, and the
# distance between a registry mistake and an unrecoverable one should not be a
# single template typo.
removable_build_dir() {
    _rbd_path="$1"
    if [ -L "$_rbd_path" ]; then
        log_warn "build-dirs: refusing symlink $_rbd_path."
        return 1
    fi
    [ -d "$_rbd_path" ] || return 1
    _rbd_real=$(CDPATH= cd -P "$_rbd_path" 2>/dev/null && pwd) || return 1
    case "$_rbd_real" in
        / | "$HOME" | "$GIT_TOPLEVEL")
            log_warn "build-dirs: refusing unsafe path $_rbd_real."
            return 1
            ;;
    esac
    # Anything containing the repository is a parent of the source, not a build
    # tree of it.
    case "$GIT_TOPLEVEL/" in
        "$_rbd_real"/*)
            log_warn "build-dirs: refusing repository ancestor $_rbd_real."
            return 1
            ;;
    esac
    return 0
}

# Resolve Main/Default Branch Name
# Usage: get_main_branch [remote_name]
get_main_branch() {
    _remote="${1:-origin}"

    # 1. The wits project registry — authoritative for a known project, below an
    # explicit git-config override but above the remote-HEAD / name guesses.
    #
    # `repo.*` is the focus repo, so this reads the focus's own `main_branch`. A
    # focus that is a *subtree* has none (it shares its anchor's git), and the
    # registry renders that as the empty string — which the `-n` test below then
    # passes over to the remote-HEAD tier rather than returning a wrong branch.
    # That tier is the right answer for such a repo anyway, and wits's own design
    # notes record a nested focus as a shape that may be removed
    # (wits/docs/reference/project-design.rst, "Open questions / future").
    if command -v wits >/dev/null 2>&1; then
        _wits_mb=$(wits project info --get repo.main_branch 2>/dev/null) &&
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

# --- Branch events ---
#
# `reference-transaction` reports ref updates, not intent. Every script that acts
# on branches has to turn the same stdin into the same answer, so the reading of
# it lives here once rather than once per script.

# True while a rebase is running in this worktree.
#
# `git rebase --abort` tears down the refs the rebase created, through a
# *committed* transaction whose new value is all zeros — from inside the hook,
# indistinguishable from `git branch -D`. The rebase state directory still exists
# at that moment, so its presence is what separates a transient teardown from a
# deletion the user meant. The state is per-worktree, hence GIT_DIR and not the
# common dir.
rebase_in_progress() {
    git_dir
    [ -d "$GIT_DIR/rebase-merge" ] || [ -d "$GIT_DIR/rebase-apply" ]
}

# The branches this transaction deleted, one `<branch> <old-sha>` per line, read
# from the transaction on stdin.
#
# Costs no subprocess, which is what keeps it usable as a first guard: an all-zero
# new value means deletion whatever the hash width, so the string test covers
# sha-1 and sha-256 without asking git for the null OID, and a symref update
# (`ref:refs/heads/x`, which HEAD gets) is not all zeros so it falls out too.
branch_deletions() {
    while read -r _bd_old _bd_new _bd_ref; do
        case "$_bd_ref" in
            refs/heads/*) ;;
            *) continue ;;
        esac
        case "$_bd_new" in
            '' | *[!0]*) continue ;;
        esac
        printf '%s %s\n' "${_bd_ref#refs/heads/}" "$_bd_old"
    done
}

# True when a deletion might really be a rename, judged from its old value alone.
# Usage: may_be_rename <old-sha>
#
# **The new name is not knowable from inside this hook**, and no amount of looking
# will change that: git deletes the old ref, fires us, and only afterwards creates
# the new one. At the moment we run, the new name exists in no ref, no reflog, not
# in HEAD's reflog and not in packed-refs. So a hook can recognise that a rename
# may have happened, and cannot learn what it was renamed to.
#
# What separates the two is whether the transaction asserted the old value.
# `git branch -m` deletes the old name with its real SHA; every ordinary deletion
# path (`git branch -d`, `git branch -D`, `git update-ref -d` without an old
# value) writes all zeros. The one other way to get a real SHA is an explicit
# `git update-ref -d <ref> <old>`, which is rare and hand-driven; treating it as
# "might be a rename" only means a stack entry outlives its branch until
# `wits stack tree prune` runs, which is the recoverable direction.
may_be_rename() {
    case "$1" in
        '' | *[!0]*) return 0 ;;
        *) return 1 ;;
    esac
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
