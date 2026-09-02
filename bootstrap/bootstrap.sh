#!/bin/sh
#
# Bootstrap: bring a machine to the state its registry entry describes.
#
#   bootstrap.sh apply <target>            do it
#   bootstrap.sh apply -n <target>         say what it would do, touch nothing
#   bootstrap.sh status                    what this machine is recorded as
#
# Selection is (capabilities the target declares) × (facts detected about the
# hardware). Order is stated outright in `order` — not derived from filenames,
# not computed from a graph. `requires` exists only so a failure can name what
# it took down with it.
#
# Nothing here may depend on wits: wits is something bootstrap *builds*, from
# a toolchain bootstrap installs. That is why this is POSIX shell and why the
# declarative files are `key: value` rather than TOML. Validation may depend
# on wits one day — it runs on a machine that is already set up — but
# execution cannot, ever.

set -u

# ==============================================================================
# Locations
# ==============================================================================

# Resolves without GNU `readlink -f`, which macOS and the BSDs do not have.
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

BOOTSTRAP_DIR=$(resolve_script_dir) || {
    printf 'Error: cannot resolve the bootstrap directory.\n' >&2
    exit 1
}
BOOTSTRAP_ROOT=$(dirname "$BOOTSTRAP_DIR")
BOOTSTRAP_SCRIPTS="$BOOTSTRAP_ROOT/scripts"
BOOTSTRAP_UNITS="$BOOTSTRAP_DIR/units"
BOOTSTRAP_REGISTRY="$BOOTSTRAP_DIR/registry"
BOOTSTRAP_ORDER="$BOOTSTRAP_DIR/order"

BOOTSTRAP_TMP=$(mktemp -d) || exit 1
cleanup() {
    rm -rf "$BOOTSTRAP_TMP"
    if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then
        sudo -k
    fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# shellcheck source=lib/meta.sh
. "$BOOTSTRAP_DIR/lib/meta.sh"
# shellcheck source=lib/unit.sh
. "$BOOTSTRAP_DIR/lib/unit.sh"
# shellcheck source=lib/env.sh
. "$BOOTSTRAP_DIR/lib/env.sh"
# shellcheck source=lib/registry.sh
. "$BOOTSTRAP_DIR/lib/registry.sh"
# shellcheck source=lib/packages.sh
. "$BOOTSTRAP_DIR/lib/packages.sh"
# shellcheck source=lib/privilege.sh
. "$BOOTSTRAP_DIR/lib/privilege.sh"
# shellcheck source=lib/state.sh
. "$BOOTSTRAP_DIR/lib/state.sh"
# shellcheck source=../scripts/detect.sh
. "$BOOTSTRAP_SCRIPTS/detect.sh"

# ==============================================================================
# Arguments
# ==============================================================================

BOOTSTRAP_DRY_RUN=0
BOOTSTRAP_FORCE=0
BOOTSTRAP_FORCE_UNITS=''
BOOTSTRAP_CAPABILITIES=''
BOOTSTRAP_HOSTNAME=''
BOOTSTRAP_PROFILE=''
BOOTSTRAP_UNIT_ROOT=''
BOOTSTRAP_ESCALATE=''
BOOTSTRAP_TARGET=''
BOOTSTRAP_VERB=''

usage() {
    cat <<EOF
Usage: bootstrap.sh apply [-n] <target>
       bootstrap.sh apply [-n] --capabilities <a,b,c>
       bootstrap.sh status

Verbs
  apply                 Bring this machine to what the target describes.
  status                Print what each unit is recorded as here.

Options
  -n, --dry-run         Resolve and report; run nothing, install nothing.
  --capabilities <list> Use this capability set instead of a registry entry,
                        for a machine not worth registering.
  --force               Ignore every state record.
  --force-unit <id>     Ignore the record for one unit. Repeatable.
  -h, --help            This.

Environment
  ROOT_PASSPHRASE       Required when any selected unit needs root and the run
                        is unprivileged.

  Everything else a run reads is declared by the unit that reads it, in
  \`units/<id>/env\`. \`apply -n <target>\` lists it for one target, says which
  names are already set, and refuses nothing.

Targets
EOF
    registry_targets | sed 's/^/  /'
}

[ $# -gt 0 ] || { usage; exit 1; }

case "$1" in
    apply|status) BOOTSTRAP_VERB=$1; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
        printf 'Unknown verb `%s`.\n\n' "$1" >&2
        usage >&2
        exit 1
        ;;
esac

while [ $# -gt 0 ]; do
    case "$1" in
        -n|--dry-run) BOOTSTRAP_DRY_RUN=1; shift ;;
        --force) BOOTSTRAP_FORCE=1; shift ;;
        --force-unit)
            [ $# -ge 2 ] || { printf -- '--force-unit needs a value.\n' >&2; exit 1; }
            BOOTSTRAP_FORCE_UNITS="$BOOTSTRAP_FORCE_UNITS $2"
            shift 2
            ;;
        --capabilities)
            [ $# -ge 2 ] || { printf -- '--capabilities needs a value.\n' >&2; exit 1; }
            BOOTSTRAP_CAPABILITIES=$(printf '%s' "$2" | tr ',' ' ')
            shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        -*)
            printf 'Unknown option `%s`.\n' "$1" >&2
            exit 1
            ;;
        *)
            [ -z "$BOOTSTRAP_TARGET" ] || {
                printf 'More than one target given: `%s` and `%s`.\n' \
                    "$BOOTSTRAP_TARGET" "$1" >&2
                exit 1
            }
            BOOTSTRAP_TARGET=$1
            shift
            ;;
    esac
done

if [ "$BOOTSTRAP_VERB" = apply ]; then
    if [ -n "$BOOTSTRAP_TARGET" ]; then
        registry_load "$BOOTSTRAP_TARGET" || exit 1
    elif [ -z "$BOOTSTRAP_CAPABILITIES" ]; then
        # Guessing which machine this is from its hostname is what the old
        # runner did, and it is circular: a machine that has never been
        # bootstrapped has whatever name its installer chose, and naming it is
        # one of the things bootstrap does.
        printf 'Name a target, or give --capabilities.\n\n' >&2
        usage >&2
        exit 1
    fi
fi

# ==============================================================================
# Facts about this machine
# ==============================================================================

BOOTSTRAP_OS=$(get_os)
BOOTSTRAP_DISTRO=''
if [ "$BOOTSTRAP_OS" = linux ]; then
    # `detect_distro` folds openSUSE's Leap and Tumbleweed into `opensuse`.
    # That is the distro naming its own releases, not one distro inheriting
    # another's package list: Ubuntu does not resolve to `debian` here, and a
    # derivative that wants support brings its own selector file.
    BOOTSTRAP_DISTRO=$(detect_distro)
fi
BOOTSTRAP_GPUS=$(detect_gpu_vendor)
BOOTSTRAP_CPU=$(detect_cpu_vendor)
BOOTSTRAP_LAPTOP=0
if is_laptop; then BOOTSTRAP_LAPTOP=1; fi

# A unit escalated with `root: yes` sees root as its own identity, so the
# account a run is *for* has to be captured before that and passed in. Units
# that modify the invoking user — group membership, their home directory —
# read this rather than asking the system who they are.
BOOTSTRAP_USER=$(id -un)

# pipx, cargo and deno all install into ~/.local/bin, and later units run what
# they installed. Putting it on PATH once here is what lets `wits` and
# `dotfiles` find their tools without each one re-exporting it.
PATH="$HOME/.local/bin:$PATH"
export PATH

# The contract a unit's script may rely on. Everything else a unit needs comes
# from the ambient environment, which `sudo -E` carries across escalation.
export BOOTSTRAP_ROOT BOOTSTRAP_SCRIPTS BOOTSTRAP_HOSTNAME BOOTSTRAP_PROFILE
export BOOTSTRAP_CAPABILITIES BOOTSTRAP_OS BOOTSTRAP_DISTRO
export BOOTSTRAP_GPUS BOOTSTRAP_CPU BOOTSTRAP_USER

# ==============================================================================
# The listed units, validated
# ==============================================================================

ordered_ids() {
    [ -f "$BOOTSTRAP_ORDER" ] || {
        printf 'Error: no order file at %s\n' "$BOOTSTRAP_ORDER" >&2
        return 1
    }
    sed 's/#.*$//; s/^[ \t]*//; s/[ \t]*$//' "$BOOTSTRAP_ORDER" | grep -v '^$'
}

ORDER_IDS="$BOOTSTRAP_TMP/order"
ordered_ids > "$ORDER_IDS" || exit 1

# Both directions, because each silence is a different bug: an id with no
# directory is a rename nobody finished, and a directory nobody listed is a
# unit that will never run and will never say so.
validate_tree() {
    _vt_bad=0

    while IFS= read -r _vt_id || [ -n "$_vt_id" ]; do
        if [ ! -d "$BOOTSTRAP_UNITS/$_vt_id" ]; then
            printf 'order lists `%s`, which has no unit directory.\n' \
                "$_vt_id" >&2
            _vt_bad=1
            continue
        fi
        unit_validate "$_vt_id" || _vt_bad=1
        env_validate "$_vt_id" || _vt_bad=1
    done < "$ORDER_IDS"

    for _vt_dir in "$BOOTSTRAP_UNITS"/*; do
        [ -d "$_vt_dir" ] || continue
        _vt_name=$(basename "$_vt_dir")
        grep -qxF "$_vt_name" "$ORDER_IDS" || {
            printf 'unit `%s` exists but `order` does not list it.\n' \
                "$_vt_name" >&2
            _vt_bad=1
        }
    done

    # A capability nothing can declare gates a unit into permanent invisibility.
    _vt_declared="$BOOTSTRAP_TMP/declared-caps"
    registry_capabilities_declared > "$_vt_declared"
    while IFS= read -r _vt_id || [ -n "$_vt_id" ]; do
        [ -d "$BOOTSTRAP_UNITS/$_vt_id" ] || continue
        for _vt_cap in $(unit_list "$_vt_id" capabilities); do
            grep -qxF "$_vt_cap" "$_vt_declared" || {
                printf 'unit %s wants capability `%s`, which no registry entry declares.\n' \
                    "$_vt_id" "$_vt_cap" >&2
                _vt_bad=1
            }
        done
    done < "$ORDER_IDS"

    return "$_vt_bad"
}

validate_tree || exit 1

# ==============================================================================
# status
# ==============================================================================

if [ "$BOOTSTRAP_VERB" = status ]; then
    printf 'State for %s\n\n' "$(state_dir)"
    while IFS= read -r id || [ -n "$id" ]; do
        if state_done "$id"; then
            printf '  %-28s recorded\n' "$id"
        elif [ -f "$(state_dir)/$id" ]; then
            printf '  %-28s recorded, but the unit changed since\n' "$id"
        else
            printf '  %-28s -\n' "$id"
        fi
    done < "$ORDER_IDS"
    exit 0
fi

# ==============================================================================
# Selection
# ==============================================================================

printf '[bootstrap] %s on %s%s\n' \
    "${BOOTSTRAP_TARGET:-ad-hoc}" "$BOOTSTRAP_OS" \
    "$([ -n "$BOOTSTRAP_DISTRO" ] && printf '/%s' "$BOOTSTRAP_DISTRO")"
printf '[bootstrap] capabilities: %s\n' "$BOOTSTRAP_CAPABILITIES"
[ "$BOOTSTRAP_DRY_RUN" -eq 1 ] &&
    printf '[bootstrap] dry run: nothing will be installed or executed\n'
printf '\n'

PLAN="$BOOTSTRAP_TMP/plan"
: > "$PLAN"
ROOT_COUNT=0

while IFS= read -r id || [ -n "$id" ]; do
    selection=$(unit_selection "$id") || exit 1
    case "$selection" in
        selected)
            printf '%s\tselected\t\n' "$id" >> "$PLAN"
            [ "$(unit_field "$id" root)" = yes ] &&
                ROOT_COUNT=$((ROOT_COUNT + 1))
            ;;
        skip:*)
            printf '%s\tskip\t%s\n' "$id" "${selection#skip:}" >> "$PLAN"
            ;;
    esac
done < "$ORDER_IDS"

# `order` is the topological statement, so a requirement further down it can
# never be satisfied in time. Checking it here is what makes a hand-written
# order safe to hand-write.
SELECTED="$BOOTSTRAP_TMP/selected"
awk -F'\t' '$2 == "selected" { print $1 }' "$PLAN" > "$SELECTED"

order_violation=0
seen="$BOOTSTRAP_TMP/seen"
: > "$seen"
while IFS= read -r id || [ -n "$id" ]; do
    for need in $(unit_list "$id" requires); do
        if ! grep -qxF "$need" "$ORDER_IDS"; then
            printf 'unit %s requires `%s`, which no unit provides.\n' \
                "$id" "$need" >&2
            order_violation=1
        elif ! grep -qxF "$need" "$seen"; then
            printf 'unit %s requires `%s`, which `order` places after it.\n' \
                "$id" "$need" >&2
            order_violation=1
        fi
    done
    printf '%s\n' "$id" >> "$seen"
done < "$ORDER_IDS"
[ "$order_violation" -eq 0 ] || exit 1

if [ "$BOOTSTRAP_FORCE" -eq 1 ]; then
    while IFS= read -r id || [ -n "$id" ]; do
        state_forget "$id"
    done < "$SELECTED"
fi
for id in $BOOTSTRAP_FORCE_UNITS; do
    state_forget "$id"
done

# After the state above has been forgotten, so a forced unit is asked for the
# inputs it will now need again, and before privilege, so a missing variable
# is not reported behind a sudo prompt.
env_prepare "$SELECTED" || exit 1

privilege_prepare "$ROOT_COUNT" || exit 1

# ==============================================================================
# Execution
# ==============================================================================

RESULTS="$BOOTSTRAP_TMP/results"
: > "$RESULTS"

# A unit is blocked when something it requires failed or was itself blocked.
# `order` being topological makes one forward pass transitively correct, which
# is the whole reason the ordering is stated rather than computed.
blocked_by() {
    for _bb_need in $(unit_list "$1" requires); do
        if awk -F'\t' -v n="$_bb_need" \
            '$1 == n && ($2 == "failed" || $2 == "blocked") { found = 1 }
             END { exit !found }' "$RESULTS"; then
            printf '%s\n' "$_bb_need"
            return 0
        fi
    done
    return 1
}

record() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS"
}

# `satisfied` and `cached` are real answers whether or not this is a dry run —
# the package diff and the `unless` probe only read. Only the doing part has to
# be reported differently, so a dry run says `plan` where a real run says
# `ran`.
DID=ran
[ "$BOOTSTRAP_DRY_RUN" -eq 1 ] && DID=plan

progress() {
    if [ "$BOOTSTRAP_DRY_RUN" -eq 1 ]; then
        printf '  %-28s would %s\n' "$1" "$2"
    else
        printf '  %-28s %s\n' "$1" "$2"
    fi
}

# Only a real run may record: writing state for work that was never done
# would make the next run skip it.
finish() {
    if [ "$BOOTSTRAP_DRY_RUN" -eq 0 ]; then
        state_record "$1" || printf '      warning: could not record state\n' >&2
    fi
    record "$1" "$DID" ''
}

run_unit() {
    _ru_id=$1

    if ! _ru_why=$(unit_available "$_ru_id"); then
        record "$_ru_id" skip "$_ru_why"
        return
    fi

    BOOTSTRAP_UNIT_ROOT=$(unit_field "$_ru_id" root)
    _ru_payload=$(unit_payload "$_ru_id")

    if [ "$(unit_field "$_ru_id" kind)" = packages ]; then
        _ru_mgr=$(packages_manager "$_ru_id") || {
            record "$_ru_id" failed 'no package manager'
            return
        }

        _ru_missing=$(packages_missing "$_ru_mgr" "$_ru_payload")
        _ru_opaque=$(packages_opaque "$_ru_payload")
        if [ -z "$_ru_missing" ] &&
                { [ -z "$_ru_opaque" ] || state_done "$_ru_id"; }; then
            record "$_ru_id" satisfied ''
            return
        fi

        progress "$_ru_id" install
        if packages_apply "$_ru_mgr" "$_ru_payload"; then
            finish "$_ru_id"
        else
            record "$_ru_id" failed \
                "$(tr '\n' ' ' < "$(packages_failures)")"
        fi
        return
    fi

    if unit_satisfied "$_ru_id"; then
        record "$_ru_id" satisfied ''
        return
    fi

    # No probe, and a record matching this content: the undecidable case that
    # state exists for.
    if state_covers "$_ru_id"; then
        record "$_ru_id" cached ''
        return
    fi

    progress "$_ru_id" run
    privilege_run_script "$_ru_payload"
    _ru_status=$?
    if [ "$_ru_status" -eq 0 ]; then
        finish "$_ru_id"
    else
        record "$_ru_id" failed "exit $_ru_status"
    fi
}

while IFS= read -r id || [ -n "$id" ]; do
    if cause=$(blocked_by "$id"); then
        record "$id" blocked "$cause"
        continue
    fi
    run_unit "$id"
done < "$SELECTED"

# ==============================================================================
# Report
# ==============================================================================

printf '\n[bootstrap] result\n'

# `PLAN` holds every considered unit, `RESULTS` the outcome for those that got
# as far as running. Concatenating in that order lets the later assignment win,
# so one status survives per unit — and resolving it into `FINAL` first means
# the listing and the counts cannot disagree.
FINAL="$BOOTSTRAP_TMP/final"
cat "$PLAN" "$RESULTS" |
    awk -F'\t' '
        NR == FNR { status[$1] = $2; detail[$1] = $3; next }
        ($0 in status) { print $0 "\t" status[$0] "\t" detail[$0] }
    ' - "$ORDER_IDS" > "$FINAL"

awk -F'\t' '{ printf "  %-28s %-10s %s\n", $1, $2, $3 }' "$FINAL"

summarise() {
    awk -F'\t' -v want="$1" '$2 == want { n++ } END { print n + 0 }' "$FINAL"
}
printf '\n[bootstrap] %s %s, %s satisfied, %s cached, %s skipped, %s failed, %s blocked\n' \
    "$(summarise "$DID")" "$DID" \
    "$(summarise satisfied)" \
    "$(summarise cached)" \
    "$(summarise skip)" \
    "$(summarise failed)" \
    "$(summarise blocked)"

if [ "$(summarise failed)" -ne 0 ] || [ "$(summarise blocked)" -ne 0 ]; then
    printf '\n[bootstrap] failed:\n'
    awk -F'\t' '$2 == "failed" { printf "  %s: %s\n", $1, $3 }' "$FINAL"
    if [ "$(summarise blocked)" -ne 0 ]; then
        printf '[bootstrap] blocked by the above:\n'
        awk -F'\t' '$2 == "blocked" { printf "  %s (needs %s)\n", $1, $3 }' \
            "$FINAL"
    fi
    printf '\n[bootstrap] re-run to retry; work already done is skipped.\n'
    exit 1
fi

printf '\n[bootstrap] done.\n'
