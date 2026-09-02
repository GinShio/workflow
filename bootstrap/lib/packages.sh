#!/bin/sh
#
# Package units: reading a platform's list, and installing what is missing.
#
# A `packages[.<platform>]` file is one selector per line, `#` for a comment.
# Three shapes, because a package manager takes three kinds of argument:
#
#   clang                  an exact name — comparable against what is installed
#   libboost_*-devel       a glob — matches an open-ended set, so not comparable
#   @pattern devel_basis   a manager directive, passed through as-is
#
# Only exact names can be diffed against the installed set, and that diff is
# what makes re-entry cheap and honest: it is not a probe someone had to
# write, it falls out of having to know what to install. Globs and directives
# are undecidable here, so the state record covers them instead.
#
# `manager` names which installer a unit's selectors are written for. It
# defaults to the platform's system manager; naming `pipx`, `cargo` or
# `flatpak` instead lets a user-level package set reuse the same diffing and
# the same failure isolation, rather than each one hand-rolling a loop that
# swallows what went wrong.
#
# Selector files are read with globbing disabled. A selector is data, and the
# shell reading it must not expand `libboost_*-devel` against the filesystem
# before the manager ever sees it.

# packages_manager <id>
#
# Kept separate from the platform token because two distros can share a
# manager, and because a user-level manager is the same everywhere.
packages_manager() {
    _pm_declared=$(unit_field "$1" manager)
    if [ -n "$_pm_declared" ]; then
        printf '%s\n' "$_pm_declared"
        return 0
    fi

    case "$BOOTSTRAP_DISTRO" in
        opensuse) printf 'zypper\n' ;;
        debian|ubuntu) printf 'apt\n' ;;
        *)
            printf 'No system package manager known for platform `%s`.\n' \
                "${BOOTSTRAP_DISTRO:-$BOOTSTRAP_OS}" >&2
            return 1
            ;;
    esac
}

# packages_installed <manager>
#
# Every package that manager already has, one name per line. Cached per
# manager for the length of the run: fifteen units asking the RPM database
# fifteen times would dominate the cost of a run with nothing left to do.
packages_installed() {
    _pi_cache="$BOOTSTRAP_TMP/installed.$1"
    if [ -f "$_pi_cache" ]; then
        cat "$_pi_cache"
        return 0
    fi

    case "$1" in
        zypper)
            rpm -qa --qf '%{NAME}\n'
            ;;
        apt)
            # Both the bare name and the multiarch-qualified one, so a
            # selector written `libssl-dev:i386` compares equal.
            # `db:Status-Status` excludes packages that are removed but still
            # have configuration on disk.
            dpkg-query -W \
                -f '${db:Status-Status} ${Package} ${Package}:${Architecture}\n' |
                awk '$1 == "installed" { print $2; print $3 }'
            ;;
        pipx)
            pipx list --short 2>/dev/null | awk '{print $1}'
            ;;
        cargo)
            cargo install --list 2>/dev/null | awk '/^[^ ]/ {print $1}'
            ;;
        flatpak)
            flatpak list --app --columns=application 2>/dev/null
            ;;
        *)
            printf 'Unknown manager `%s`.\n' "$1" >&2
            return 1
            ;;
    esac | sort -u > "$_pi_cache"

    cat "$_pi_cache"
}

# packages_install <manager> <selector>...
#
# One invocation of the manager. Globbing stays disabled; the caller holds
# selectors that must reach the manager unexpanded.
packages_install() {
    _pin_mgr=$1
    shift

    case "$_pin_mgr" in
        zypper)
            bootstrap_run zypper --non-interactive install \
                --auto-agree-with-licenses "$@"
            ;;
        apt)
            DEBIAN_FRONTEND=noninteractive \
                bootstrap_run apt-get install -y "$@"
            ;;
        pipx)
            bootstrap_run pipx install "$@"
            ;;
        cargo)
            bootstrap_run cargo install --locked "$@"
            ;;
        flatpak)
            bootstrap_run flatpak install -y flathub "$@"
            ;;
        *)
            printf 'Unknown manager `%s`.\n' "$_pin_mgr" >&2
            return 1
            ;;
    esac
}

# packages_install_directive <manager> <directive>
#
# A `@`-prefixed line. Each manager defines its own vocabulary; an unknown one
# fails rather than being ignored, because a silently dropped directive is a
# package set that never installs and never says why.
packages_install_directive() {
    _pid_verb=${2%% *}
    _pid_rest=${2#* }

    if [ "$1" = zypper ] && [ "$_pid_verb" = '@pattern' ]; then
        # shellcheck disable=SC2086
        bootstrap_run zypper --non-interactive install \
            --auto-agree-with-licenses -t pattern $_pid_rest
        return $?
    fi

    printf 'Unknown directive `%s` for %s.\n' "$_pid_verb" "$1" >&2
    return 1
}

# packages_selectors <file> <shape>
#
# The selectors of one shape: `exact`, `glob` or `directive`.
packages_selectors() {
    awk -v shape="$2" '
        { sub(/[ \t]*#.*$/, "") }
        {
            gsub(/^[ \t]+|[ \t]+$/, "")
            if ($0 == "") next
            if (substr($0, 1, 1) == "@") { if (shape == "directive") print; next }
            if (index($0, "*") > 0)      { if (shape == "glob")      print; next }
            if (shape == "exact") print
        }
    ' "$1"
}

# packages_missing <manager> <file>
#
# The exact-name selectors this machine does not have. Empty output means
# every decidable part of the unit is in place.
#
# The comparison key drops an extras suffix — pipx takes
# `iree-base-compiler[onnx]` but reports `iree-base-compiler` — so the diff
# does not reinstall on every run.
packages_missing() {
    _pmi_wanted="$BOOTSTRAP_TMP/wanted"
    packages_selectors "$2" exact |
        awk '{ key = $0; sub(/\[.*$/, "", key); print key "\t" $0 }' |
        sort -u > "$_pmi_wanted"
    [ -s "$_pmi_wanted" ] || return 0

    packages_installed "$1" |
        awk -v want="$_pmi_wanted" '
            BEGIN { while ((getline line < want) > 0) {
                        split(line, f, "\t"); sel[f[1]] = f[2] } }
            { delete sel[$0] }
            END { for (k in sel) print sel[k] }
        ' | sort
}

# packages_opaque <file>
#
# The selectors whose state cannot be read off the machine.
packages_opaque() {
    packages_selectors "$1" glob
    packages_selectors "$1" directive
}

# packages_apply <manager> <file>
#
# Install everything missing. Returns 1 when any selector could not be
# installed and leaves those selectors in the file `packages_failures` names.
#
# The failures go to a file rather than to stdout because stdout belongs to
# the manager: capturing it to read a return value would swallow every line of
# install progress the run is there to show.
#
# A failed batch is retried one selector at a time. A single bad name fails
# the whole invocation — the most common way a run breaks here — and without
# the retry the report would blame ninety packages for one typo. The retry
# doubles the work, but only on a path that was already broken.
packages_apply() {
    _pa_mgr=$1
    _pa_file=$2
    _pa_todo="$BOOTSTRAP_TMP/todo"
    _pa_failed=$(packages_failures)
    : > "$_pa_failed"

    set -f
    {
        packages_missing "$_pa_mgr" "$_pa_file"
        packages_selectors "$_pa_file" glob
    } > "$_pa_todo"

    if [ -s "$_pa_todo" ]; then
        # shellcheck disable=SC2046
        if ! packages_install "$_pa_mgr" $(cat "$_pa_todo"); then
            printf 'Batch install failed; isolating selectors.\n' >&2
            while IFS= read -r _pa_sel || [ -n "$_pa_sel" ]; do
                [ -n "$_pa_sel" ] || continue
                packages_install "$_pa_mgr" "$_pa_sel" ||
                    printf '%s\n' "$_pa_sel" >> "$_pa_failed"
            done < "$_pa_todo"
        fi
    fi

    packages_selectors "$_pa_file" directive > "$BOOTSTRAP_TMP/directives"
    while IFS= read -r _pa_dir || [ -n "$_pa_dir" ]; do
        [ -n "$_pa_dir" ] || continue
        packages_install_directive "$_pa_mgr" "$_pa_dir" ||
            printf '%s\n' "$_pa_dir" >> "$_pa_failed"
    done < "$BOOTSTRAP_TMP/directives"
    set +f

    [ ! -s "$_pa_failed" ]
}

# packages_failures
#
# Where `packages_apply` leaves the selectors it could not install.
packages_failures() {
    printf '%s/pkg-failed\n' "$BOOTSTRAP_TMP"
}
