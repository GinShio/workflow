#!/bin/sh

# Git-hooks dispatch engine.
#
# Sourced by core/runner *after* lib.sh; the hook scripts under <hook>.d/ never
# need it. This is the part that decides *what runs* — the enable/disable
# hierarchy and the base / overlay / external / repo-local layers — and enforces
# the fail-fast rule (the first non-zero exit stops the hook). It leans on
# lib.sh for is_truthy, the cfg_* resolvers, and logging.

# Is a hook, or a specific script within it, enabled? Answers to the disable
# hierarchy: global, per-hook, per-script (by clean name). The global and per-hook
# switches both apply to the whole run and are checked identically, so lib.sh
# resolves them once (env overrides config) into the single flag _WITS_HOOKS_OFF
# and this hot path never re-forks git for them; only the per-script key varies
# per candidate and is read live via cfg_bool.
is_enabled() {
    _hook="$1"
    _script="$2"

    # Whole-run kill switch (global or per-hook), resolved once in lib.sh.
    [ "${_WITS_HOOKS_OFF:-0}" = 1 ] && return 1

    # Script level, addressed by clean name (numeric prefix stripped). The script
    # name varies per candidate, so this one stays a live lookup.
    if [ -n "$_script" ]; then
        _clean=$(echo "$_script" | sed -E 's/^[0-9]+-//')
        cfg_bool "wits.hooks.$_hook.$_clean-disable" && return 1
    fi

    return 0
}

# A candidate is runnable only if it starts with a shebang. Anything else is a
# data file or an encrypted blob (transcrypt) sitting in the hooks tree, and is
# skipped rather than executed. Reading the first line with the shell builtin
# avoids a `dd`/`head` fork per candidate; only the leading `#!` is inspected.
check_script_header() {
    IFS= read -r _hdr < "$1" 2>/dev/null
    case "$_hdr" in
        "#!"*) return 0 ;;
        *) return 1 ;;
    esac
}

# Execute one hook script, feeding it the hook's buffered stdin when there is
# any, and enforce the fail-fast rule: a non-zero exit is logged (with <label>)
# and aborts the whole hook with that status. This is the single place the
# run/redirect/check-and-exit dance lives; every layer below calls it.
# Usage: run_script <label> <script> <stdin_source> [args...]
run_script() {
    _rs_label="$1"
    _rs_script="$2"
    _rs_stdin="$3"
    shift 3

    if [ -n "$_rs_stdin" ] && [ -f "$_rs_stdin" ]; then
        "$_rs_script" "$@" < "$_rs_stdin"
    else
        "$_rs_script" "$@"
    fi
    _rs_code=$?
    if [ "$_rs_code" -ne 0 ]; then
        log_error "$_rs_label failed with exit code $_rs_code"
        exit "$_rs_code"
    fi
}

# Run every enabled, executable script in a directory, in filename order,
# stopping the whole hook on the first failure.
run_hook_dir() {
    dir_path="$1"
    hook_name="$2"
    stdin_source="$3"
    shift 3

    [ -d "$dir_path" ] || return 0

    for script in "$dir_path"/*; do
        [ -f "$script" ] && [ -x "$script" ] || continue
        script_base=$(basename "$script")

        is_enabled "$hook_name" "$script_base" || continue

        if ! check_script_header "$script"; then
            log_debug "Skipping '$script_base': no shebang (data file or encrypted)."
            continue
        fi

        run_script "Hook script '$script_base'" "$script" "$stdin_source" "$@"
    done
}

# Run the base <hook>.d directory, then any secret-* overlay layers beside it.
# Usage: run_hook_overlays <hooks_root_dir> <hook_name> <stdin_source> [args...]
run_hook_overlays() {
    _base_root="$1"
    _hook_name="$2"
    _stdin_source="$3"
    shift 3

    run_hook_dir "${_base_root}/${_hook_name}.d" "$_hook_name" "$_stdin_source" "$@"

    for _domain_root in "$_base_root"/secret-*; do
        if [ -d "$_domain_root" ]; then
            _layer_dir="${_domain_root}/${_hook_name}.d"
            if [ -d "$_layer_dir" ]; then
                log_debug "Executing overlay layer: $(basename "$_domain_root")"
                run_hook_dir "$_layer_dir" "$_hook_name" "$_stdin_source" "$@"
            fi
        fi
    done
}

# Run project-local external hooks: directories to scan (Husky/.githooks, both
# single-file and split .d forms) and explicit script mappings.
# Usage: run_external_hooks <hook_name> <stdin_source> [args...]
run_external_hooks() {
    _ext_hook_name="$1"
    _ext_stdin_source="$2"
    shift 2

    # 1. Directory scanning. cfg_value applies the standard env-over-config rule.
    _ext_dirs=$(cfg_value wits.hooks.external-dirs)
    if [ -n "$_ext_dirs" ]; then
        git_toplevel   # relative external dirs are resolved against the work tree
        _old_ifs="$IFS"
        IFS=":"
        set -f
        for _dir in $_ext_dirs; do
            IFS="$_old_ifs"
            set +f

            if [ -n "$_dir" ]; then
                case "$_dir" in
                    /*) _resolved_dir="$_dir" ;;
                    *)  _resolved_dir="${GIT_TOPLEVEL}/${_dir}" ;;
                esac

                if [ -d "$_resolved_dir" ]; then
                    log_debug "Scanning external hooks directory: $_resolved_dir"

                    _ext_script="$_resolved_dir/$_ext_hook_name"
                    if [ -f "$_ext_script" ] && [ -x "$_ext_script" ]; then
                        if is_enabled "$_ext_hook_name" "external"; then
                            log_info "Running external hook script: $_ext_script"
                            run_script "External hook '$_ext_script'" \
                                "$_ext_script" "$_ext_stdin_source" "$@"
                        fi
                    fi

                    _ext_dir_d="$_resolved_dir/${_ext_hook_name}.d"
                    if [ -d "$_ext_dir_d" ]; then
                        log_debug "Executing external hook dir: $_ext_dir_d"
                        run_hook_dir "$_ext_dir_d" "$_ext_hook_name" "$_ext_stdin_source" "$@"
                    fi
                fi
            fi

            IFS=":"
            set -f
        done
        IFS="$_old_ifs"
        set +f
    fi

    # 2. Explicit script mapping (e.g. scripts/lint.sh). cfg_value handles the
    #    env twin (WITS_HOOKS_<HOOK>_EXTERNAL_SCRIPTS) and config in one call.
    _ext_scripts=$(cfg_value "wits.hooks.${_ext_hook_name}.external-scripts")

    if [ -n "$_ext_scripts" ]; then
        git_toplevel   # relative script paths are resolved against the work tree
        _old_ifs="$IFS"
        IFS=":"
        set -f
        for _script in $_ext_scripts; do
            IFS="$_old_ifs"
            set +f

            if [ -n "$_script" ]; then
                case "$_script" in
                    /*) _resolved_script="$_script" ;;
                    *)  _resolved_script="${GIT_TOPLEVEL}/${_script}" ;;
                esac

                if [ -f "$_resolved_script" ] && [ -x "$_resolved_script" ]; then
                    _script_base=$(basename "$_resolved_script")
                    if is_enabled "$_ext_hook_name" "$_script_base"; then
                        log_info "Running explicit external script: $_resolved_script"
                        run_script "Explicit external script '$_resolved_script'" \
                            "$_resolved_script" "$_ext_stdin_source" "$@"
                    fi
                elif [ ! -e "$_resolved_script" ]; then
                    log_warn "Explicit external script not found: $_resolved_script"
                fi
            fi

            IFS=":"
            set -f
        done
        IFS="$_old_ifs"
        set +f
    fi
}

# Run a legacy hook the repository installed the old-fashioned way, so switching
# core.hooksPath to this framework never silently drops it.
run_local_hook() {
    hook_name="$1"
    stdin_source="$2"
    shift 2
    git_dir
    local_hook_script="$GIT_DIR/hooks/$hook_name"

    if [ -f "$local_hook_script" ] && [ -x "$local_hook_script" ]; then
        if is_enabled "$hook_name" "local"; then
            run_script "Local hook '$hook_name'" "$local_hook_script" "$stdin_source" "$@"
        fi
    fi
}
