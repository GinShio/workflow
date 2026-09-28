#!/bin/sh
#
# Service runner: run the units a trigger names, where they apply.
#
#   runner.sh <type>        run them
#   runner.sh -n <type>     say what would run and why the rest would not
#
# <type> is the trigger — `autostart` or `nightly` — and a unit belongs to the
# one its `runs` names. Whether it runs on this machine is decided by its own
# conditions: `enable`, the facts in `when`, a payload for this platform, the
# commands in `optional`, `power`, and `every`. `order` states the sequence.
#
# What differs between machines is declared, not coded: the declarations that
# dotfiles deploys for this machine override `enable`, `every` and `power`, and
# set the parameters a unit lists in `params`. README.md beside this file
# describes both formats.

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

SERVICES_DIR=$(resolve_script_dir) || {
    printf 'Error: cannot resolve the services directory.\n' >&2
    exit 1
}
SERVICES_ROOT=$(dirname "$SERVICES_DIR")
SERVICES_UNITS="$SERVICES_DIR/units"
SERVICES_ORDER="$SERVICES_DIR/order"
SERVICES_STATE="${XDG_STATE_HOME:-$HOME/.local/state}/wits/services/last-run"
SERVICES_DECLARED="${XDG_CONFIG_HOME:-$HOME/.config}/wits/services"

# One per systemd user unit that invokes this runner (dotfiles/systemd).
SERVICES_TYPES='autostart nightly'
UNIT_KEYS='runs every power when optional enable params'
# The unit keys a declaration may override. The rest describe what a unit is,
# and are the same on every machine.
DECLARABLE_KEYS='enable every power'

TAB=$(printf '\t')

# The environment dotfiles deploys for this machine: payloads read the overlay
# list from it, and `sudo -A` finds its askpass helper there.
if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/wits/.env" ]; then
    . "${XDG_CONFIG_HOME:-$HOME/.config}/wits/.env"
fi
export DOTFILES_OVERLAYS

# shellcheck source=../scripts/meta.sh
. "$SERVICES_ROOT/scripts/meta.sh"
# shellcheck source=../scripts/detect.sh
. "$SERVICES_ROOT/scripts/detect.sh"

# ==============================================================================
# Arguments
# ==============================================================================

usage() {
    cat <<EOF
Usage: runner.sh [-n] <type>

  <type>          One of: $SERVICES_TYPES
  -n, --dry-run   Say what would run and why the rest would not. Runs
                  nothing and records nothing.
EOF
}

# is_word <word> <list>
#
# Whether <word> is one of the words of <list>, compared whole, so that a
# value holding two words never matches.
is_word() {
    for _iw_word in $2; do
        [ "$1" = "$_iw_word" ] && return 0
    done
    return 1
}

SERVICES_DRY_RUN=0
case "${1:-}" in
    -n|--dry-run) SERVICES_DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
esac
[ $# -eq 1 ] || { usage >&2; exit 1; }
SERVICES_RUN=$1
if ! is_word "$SERVICES_RUN" "$SERVICES_TYPES"; then
    printf 'Unknown type `%s`.\n\n' "$SERVICES_RUN" >&2
    usage >&2
    exit 1
fi

# The run's one clock: every unit is judged due, and recorded as run, at the
# moment the run started, so the time the units before it take never moves a
# unit's schedule.
SERVICES_STARTED=$(date +%s)

SERVICES_TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$SERVICES_TMP"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# ==============================================================================
# Facts about this machine
# ==============================================================================

SERVICES_OS=$(get_os)
SERVICES_DISTRO=''
if [ "$SERVICES_OS" = linux ]; then
    SERVICES_DISTRO=$(detect_distro)
fi
SERVICES_GPUS=$(detect_gpu_vendor)
SERVICES_LAPTOP=0
if is_laptop; then SERVICES_LAPTOP=1; fi
SERVICES_ON_AC=0
if is_on_ac; then SERVICES_ON_AC=1; fi

export SERVICES_ROOT

# ==============================================================================
# Units
# ==============================================================================

unit_file() {
    printf '%s\n' "$SERVICES_UNITS/$1/unit"
}

unit_params() {
    meta_list "$(unit_file "$1")" params | tr '\n' ' '
}

# every_parse <every>
#
# Prints `<count> <d|h|m>`, or fails on a value that is not an interval.
every_parse() {
    case "$1" in
        daily) printf '1 d\n' ;;
        weekly) printf '7 d\n' ;;
        monthly) printf '30 d\n' ;;
        [1-9]*[dhm])
            _ep_count=${1%?}
            case "$_ep_count" in
                *[!0-9]*) return 1 ;;
            esac
            printf '%s %s\n' "$_ep_count" "${1#"$_ep_count"}"
            ;;
        *) return 1 ;;
    esac
}

# setting_problem <key> <value>
#
# Why <value> is not legal for a declarable key, or nothing when it is. A
# unit's own value and a declared one are both checked here, so the two
# cannot come to disagree about what is legal. An empty `every` or `power`
# means no such condition; `enable` has no empty meaning.
setting_problem() {
    case "$1" in
        every)
            if [ -n "$2" ] && ! every_parse "$2" >/dev/null; then
                printf '`every` takes daily, weekly, monthly or <N>d|h|m, not `%s`\n' "$2"
            fi
            ;;
        power)
            case "$2" in
                ''|ac) ;;
                *) printf '`power` takes `ac` or nothing, not `%s`\n' "$2" ;;
            esac
            ;;
        enable)
            case "$2" in
                yes|no) ;;
                *) printf '`enable` takes `yes` or `no`, not `%s`\n' "$2" ;;
            esac
            ;;
    esac
}

# unit_payload <id>
#
# The payload this machine runs, or nothing when the unit has none for it.
# Resolved the way bootstrap resolves its payloads: the distro file, then the
# OS file, then the generic one. A derivative inherits nothing, so Ubuntu does
# not run `run.debian`.
unit_payload() {
    _up_base="$SERVICES_UNITS/$1/run"
    if [ -n "$SERVICES_DISTRO" ] && [ -f "$_up_base.$SERVICES_DISTRO" ]; then
        printf '%s\n' "$_up_base.$SERVICES_DISTRO"
    elif [ -f "$_up_base.$SERVICES_OS" ]; then
        printf '%s\n' "$_up_base.$SERVICES_OS"
    elif [ -f "$_up_base" ]; then
        printf '%s\n' "$_up_base"
    fi
}

# unit_validate <id>
#
# Rejects metadata that cannot mean what it says. Each of these would
# otherwise surface as a unit that silently never runs.
unit_validate() {
    _uv_file=$(unit_file "$1")
    if [ ! -f "$_uv_file" ]; then
        printf 'unit %s: no `unit` file\n' "$1" >&2
        return 1
    fi

    # One key per line, not per word: `every power: 5d` is one misspelled key,
    # not two good ones.
    _uv_bad=0
    meta_keys "$_uv_file" > "$SERVICES_TMP/keys"
    while IFS= read -r _uv_key <&5; do
        if ! is_word "$_uv_key" "$UNIT_KEYS"; then
            printf 'unit %s: unknown key `%s`\n' "$1" "$_uv_key" >&2
            _uv_bad=1
        fi
    done 5< "$SERVICES_TMP/keys"

    _uv_runs=$(meta_field "$_uv_file" runs)
    if [ -z "$_uv_runs" ]; then
        printf 'unit %s: missing `runs`\n' "$1" >&2
        _uv_bad=1
    elif ! is_word "$_uv_runs" "$SERVICES_TYPES"; then
        printf 'unit %s: `runs` takes one of: %s\n' "$1" "$SERVICES_TYPES" >&2
        _uv_bad=1
    fi

    # An absent key is the default, so only a value the unit gives is checked.
    for _uv_key in $DECLARABLE_KEYS; do
        _uv_value=$(meta_field "$_uv_file" "$_uv_key")
        [ -n "$_uv_value" ] || continue
        _uv_problem=$(setting_problem "$_uv_key" "$_uv_value")
        if [ -n "$_uv_problem" ]; then
            printf 'unit %s: %s\n' "$1" "$_uv_problem" >&2
            _uv_bad=1
        fi
    done

    # A parameter reaches the payload as SERVICES_PARAM_<NAME>, so its name
    # has to survive becoming an environment variable name.
    for _uv_param in $(meta_list "$_uv_file" params); do
        case "$_uv_param" in
            [!a-z]*|*[!a-z0-9-]*)
                printf 'unit %s: parameter `%s` must be lower-case letters, digits and `-`\n' \
                    "$1" "$_uv_param" >&2
                _uv_bad=1
                ;;
        esac
        if is_word "$_uv_param" "$DECLARABLE_KEYS"; then
            printf 'unit %s: parameter `%s` would shadow the key of that name\n' \
                "$1" "$_uv_param" >&2
            _uv_bad=1
        fi
    done

    # The kind of fact is checked, not the vendor a `gpu:` fact names. That
    # list is whatever detect.sh reports; mirroring it here would cost more
    # than the rare typo it catches, which reads as a fact that does not hold.
    for _uv_fact in $(meta_list "$_uv_file" when); do
        case "$_uv_fact" in
            gpu:?*|hw:laptop) ;;
            *)
                printf 'unit %s: unknown fact `%s`\n' "$1" "$_uv_fact" >&2
                _uv_bad=1
                ;;
        esac
    done

    # Tested one path at a time because an unmatched glob stays literal, and
    # the `-f` on that literal is what turns "no platform variants" into a
    # clean negative.
    _uv_found=0
    for _uv_path in "$SERVICES_UNITS/$1/run" "$SERVICES_UNITS/$1/run".*; do
        if [ -f "$_uv_path" ]; then
            _uv_found=1
            break
        fi
    done
    if [ "$_uv_found" -eq 0 ]; then
        printf 'unit %s: no `run` payload for any platform\n' "$1" >&2
        _uv_bad=1
    fi

    return "$_uv_bad"
}

ORDER_IDS="$SERVICES_TMP/order"
if [ ! -f "$SERVICES_ORDER" ]; then
    printf 'Error: no order file at %s\n' "$SERVICES_ORDER" >&2
    exit 1
fi
sed 's/#.*$//; s/^[[:blank:]]*//; s/[[:blank:]]*$//' "$SERVICES_ORDER" |
    grep -v '^$' > "$ORDER_IDS"

# Both directions, because each silence is a different bug: an id with no
# directory is a rename nobody finished, and a directory nobody listed is a
# unit that will never run and will never say so.
validate_tree() {
    _vt_bad=0

    for _vt_id in $(sort "$ORDER_IDS" | uniq -d); do
        printf 'order lists `%s` more than once.\n' "$_vt_id" >&2
        _vt_bad=1
    done

    while IFS= read -r _vt_id <&3; do
        if [ ! -d "$SERVICES_UNITS/$_vt_id" ]; then
            printf 'order lists `%s`, which has no unit directory.\n' \
                "$_vt_id" >&2
            _vt_bad=1
            continue
        fi
        unit_validate "$_vt_id" || _vt_bad=1
    done 3< "$ORDER_IDS"

    for _vt_dir in "$SERVICES_UNITS"/*; do
        [ -d "$_vt_dir" ] || continue
        _vt_name=$(basename "$_vt_dir")
        grep -qxF "$_vt_name" "$ORDER_IDS" || {
            printf 'unit `%s` exists but `order` does not list it.\n' \
                "$_vt_name" >&2
            _vt_bad=1
        }
    done

    return "$_vt_bad"
}

validate_tree || exit 1

# ==============================================================================
# Declarations
# ==============================================================================

# declaration_files
#
# `<layer><TAB><file>` in reading order: one file per overlay this machine
# deploys, in the order it lists them, then the machine's own.
#
# Named after the overlay and found through the overlay list rather than a
# glob: taking an overlay off a machine does not delete the file it deployed,
# and a glob would go on reading the stale declarations.
declaration_files() {
    _df_old_ifs=$IFS
    IFS=:
    set -f
    for _df_overlay in ${DOTFILES_OVERLAYS:-}; do
        if [ -n "$_df_overlay" ]; then
            printf '%s\t%s\n' "$_df_overlay" "$SERVICES_DECLARED/$_df_overlay.conf"
        fi
    done
    set +f
    IFS=$_df_old_ifs
    printf 'host\t%s\n' "$SERVICES_DECLARED/host.conf"
}

DECLARED="$SERVICES_TMP/declared"
LAYERS_READ="$SERVICES_TMP/layers-read"
: > "$DECLARED"
: > "$LAYERS_READ"

# load_declaration_file <layer> <file>
#
# Appends every assignment the file makes to DECLARED as
# `<unit><TAB><key><TAB><op><TAB><value><TAB><layer>`, where <op> is `=` to
# replace and `+` to append. Reports each assignment it rejects.
load_declaration_file() {
    _ldf_bad=0

    # A line with no colon is not an assignment, and the parser would skip it,
    # dropping whatever it meant without a word.
    if ! awk '
        /^[ \t]*(#|$)/ { next }
        index($0, ":") == 0 {
            printf "%s:%d: not a `key: value` line\n", FILENAME, NR
            bad = 1
        }
        END { exit bad }
    ' "$2" >&2; then
        _ldf_bad=1
    fi

    # Values are words, so a tab between them means what a space does; it
    # becomes one because DECLARED is itself tab-separated.
    meta_pairs "$2" |
        awk -F'\t' '{ k = $1; sub(/^[^\t]*\t/, ""); gsub(/\t/, " "); print k "\t" $0 }' \
        > "$SERVICES_TMP/pairs"
    while IFS="$TAB" read -r _ldf_key _ldf_value <&4; do
        case "$_ldf_key" in
            ?*.?*) ;;
            *)
                printf '%s: `%s` is not `<unit>.<key>`\n' "$2" "$_ldf_key" >&2
                _ldf_bad=1
                continue
                ;;
        esac
        _ldf_id=${_ldf_key%%.*}
        _ldf_name=${_ldf_key#*.}
        _ldf_op='='
        case "$_ldf_name" in
            *+) _ldf_op='+'; _ldf_name=${_ldf_name%+} ;;
        esac

        if ! grep -qxF "$_ldf_id" "$ORDER_IDS"; then
            printf '%s: no unit `%s`\n' "$2" "$_ldf_id" >&2
            _ldf_bad=1
            continue
        fi

        if is_word "$_ldf_name" "$DECLARABLE_KEYS"; then
            if [ "$_ldf_op" = '+' ]; then
                printf '%s: `%s` takes one value; it cannot be appended to\n' \
                    "$2" "$_ldf_key" >&2
                _ldf_bad=1
                continue
            fi
            _ldf_problem=$(setting_problem "$_ldf_name" "$_ldf_value")
            if [ -n "$_ldf_problem" ]; then
                printf '%s: %s: %s\n' "$2" "$_ldf_key" "$_ldf_problem" >&2
                _ldf_bad=1
                continue
            fi
        elif is_word "$_ldf_name" "$UNIT_KEYS"; then
            printf '%s: `%s` is the same on every machine; a declaration sets %s, or a parameter\n' \
                "$2" "$_ldf_key" "$(printf '%s' "$DECLARABLE_KEYS" | sed 's/ /, /g')" >&2
            _ldf_bad=1
            continue
        else
            _ldf_params=$(unit_params "$_ldf_id")
            _ldf_params=${_ldf_params% }
            if ! is_word "$_ldf_name" "$_ldf_params"; then
                printf '%s: unit `%s` has no parameter `%s` (it has: %s)\n' \
                    "$2" "$_ldf_id" "$_ldf_name" "${_ldf_params:-none}" >&2
                _ldf_bad=1
                continue
            fi
        fi

        printf '%s\t%s\t%s\t%s\t%s\n' "$_ldf_id" "$_ldf_name" "$_ldf_op" \
            "$_ldf_value" "$1" >> "$DECLARED"
    done 4< "$SERVICES_TMP/pairs"

    return "$_ldf_bad"
}

# Every declaration is checked before any unit runs: a misspelled
# `<unit>.enable: no` that was merely ignored would leave running the very
# unit it was written to stop.
load_declarations() {
    case ":${DOTFILES_OVERLAYS:-}:" in
        *:host:*)
            printf 'Error: an overlay named `host` would share %s/host.conf with the host layer.\n' \
                "$SERVICES_DECLARED" >&2
            return 1
            ;;
    esac

    _ld_bad=0
    declaration_files > "$SERVICES_TMP/layers"
    while IFS="$TAB" read -r _ld_layer _ld_file <&4; do
        [ -f "$_ld_file" ] || continue
        printf '%s\n' "$_ld_layer" >> "$LAYERS_READ"
        load_declaration_file "$_ld_layer" "$_ld_file" || _ld_bad=1
    done 4< "$SERVICES_TMP/layers"
    return "$_ld_bad"
}

load_declarations || exit 1

# declared <unit> <key>
#
# `<value><TAB><origin>` for a key some layer declares, nothing otherwise.
# Layers apply in reading order: `=` replaces what came before it, `+`
# appends to it. <origin> names the layers the value came from, an appending
# one marked `+`.
declared() {
    awk -F'\t' -v id="$1" -v key="$2" '
        $1 == id && $2 == key {
            if ($3 == "=") {
                value = $4
                origin = $5
            } else {
                value = value == "" ? $4 : value " " $4
                origin = origin == "" ? $5 "+" : origin ", " $5 "+"
            }
            seen = 1
        }
        END { if (seen) print value "\t" origin }
    ' "$DECLARED"
}

# unit_setting <id> <key>
#
# The value a unit runs with for a declarable key: the declared one when a
# layer sets it, the unit's own otherwise.
unit_setting() {
    _ust_declared=$(declared "$1" "$2")
    if [ -n "$_ust_declared" ]; then
        printf '%s\n' "${_ust_declared%%"$TAB"*}"
    else
        meta_field "$(unit_file "$1")" "$2"
    fi
}

# setting_origin <id> <key>
#
# ` (<origin>)` when a declaration set the key, nothing when the unit did.
setting_origin() {
    _so_declared=$(declared "$1" "$2")
    if [ -n "$_so_declared" ]; then
        printf ' (%s)\n' "${_so_declared#*"$TAB"}"
    fi
}

param_env_name() {
    printf 'SERVICES_PARAM_%s\n' \
        "$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]' | tr '-' '_')"
}

# ==============================================================================
# Selection
# ==============================================================================

fact_holds() {
    case "$1" in
        gpu:any)
            [ -n "$SERVICES_GPUS" ]
            ;;
        gpu:*)
            case " $SERVICES_GPUS " in
                *" ${1#gpu:} "*) return 0 ;;
                *) return 1 ;;
            esac
            ;;
        hw:laptop)
            [ "$SERVICES_LAPTOP" -eq 1 ]
            ;;
    esac
}

epoch_date() {
    date -d "@$1" '+%F %T' 2>/dev/null ||
        date -r "$1" '+%F %T' 2>/dev/null ||
        printf '%s\n' "$1"
}

# schedule_due <id> <every>
#
# Exit 0 when the unit is due, 1 when it is not (printing the start of the
# run it last succeeded in), 2 when its record cannot be read.
schedule_due() {
    _sd_record="$SERVICES_STATE/$1"
    [ -f "$_sd_record" ] || return 0

    _sd_last=$(cat "$_sd_record")
    case "$_sd_last" in
        ''|*[!0-9]*) return 2 ;;
    esac
    _sd_now=$SERVICES_STARTED
    # shellcheck disable=SC2046
    set -- $(every_parse "$2")

    if [ "$2" = d ]; then
        # Whole days between the two UTC midnights rather than elapsed
        # seconds: two nightly runs start a day apart only to within the
        # timer's accuracy, and a few seconds short must not read as less
        # than a day. The day boundary is UTC midnight, not local midnight.
        _sd_days=$(( (_sd_now - _sd_now % 86400 - _sd_last + _sd_last % 86400) / 86400 ))
        [ "$_sd_days" -ge "$1" ] && return 0
    else
        _sd_span=$(( $1 * 60 ))
        [ "$2" = h ] && _sd_span=$(( $1 * 3600 ))
        [ $(( _sd_now - _sd_last )) -ge "$_sd_span" ] && return 0
    fi
    printf '%s\n' "$_sd_last"
    return 1
}

schedule_record() {
    mkdir -p "$SERVICES_STATE" || return 1
    _sr_tmp=$(mktemp "$SERVICES_STATE/.$1.XXXXXX") || return 1
    if printf '%s\n' "$SERVICES_STARTED" > "$_sr_tmp" &&
        mv "$_sr_tmp" "$SERVICES_STATE/$1"; then
        return 0
    fi
    rm -f "$_sr_tmp"
    return 1
}

# unit_selection <id>
#
# Prints `selected`, `skip:<reason>` naming the first condition that failed,
# or `failed:<reason>` when a condition could not be evaluated.
unit_selection() {
    _us_file=$(unit_file "$1")

    if [ "$(unit_setting "$1" enable)" = no ]; then
        printf 'skip:disabled%s\n' "$(setting_origin "$1" enable)"
        return
    fi

    for _us_fact in $(meta_list "$_us_file" when); do
        if ! fact_holds "$_us_fact"; then
            printf 'skip:%s does not hold\n' "$_us_fact"
            return
        fi
    done

    if [ -z "$(unit_payload "$1")" ]; then
        printf 'skip:no payload for %s\n' "${SERVICES_DISTRO:-$SERVICES_OS}"
        return
    fi

    for _us_cmd in $(meta_list "$_us_file" optional); do
        if ! command -v "$_us_cmd" >/dev/null 2>&1; then
            printf 'skip:%s not present\n' "$_us_cmd"
            return
        fi
    done

    if [ "$(unit_setting "$1" power)" = ac ] && [ "$SERVICES_ON_AC" -eq 0 ]; then
        printf 'skip:on battery\n'
        return
    fi

    _us_every=$(unit_setting "$1" every)
    if [ -n "$_us_every" ]; then
        _us_last=$(schedule_due "$1" "$_us_every")
        case $? in
            0) ;;
            1)
                printf 'skip:not due, last run %s, every %s%s\n' \
                    "$(epoch_date "$_us_last")" "$_us_every" \
                    "$(setting_origin "$1" every)"
                return
                ;;
            *)
                printf 'failed:unreadable schedule record %s\n' \
                    "$SERVICES_STATE/$1"
                return
                ;;
        esac
    fi

    printf 'selected\n'
}

# ==============================================================================
# Execution
# ==============================================================================

printf '[services] %s on %s%s\n' "$SERVICES_RUN" "$SERVICES_OS" \
    "${SERVICES_DISTRO:+/$SERVICES_DISTRO}"
if [ -s "$LAYERS_READ" ]; then
    printf '[services] declarations: %s\n' \
        "$(paste -s -d, "$LAYERS_READ" | sed 's/,/, /g')"
else
    printf '[services] declarations: none in %s\n' "$SERVICES_DECLARED"
fi
[ "$SERVICES_DRY_RUN" -eq 1 ] &&
    printf '[services] dry run: nothing will be executed or recorded\n'

RESULTS="$SERVICES_TMP/results"
: > "$RESULTS"
record() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS"
}

# record_declared <id>
#
# One report line per parameter of the unit, and per declarable key a layer
# overrode, with the layers the value came from.
record_declared() {
    _rd_params=$(unit_params "$1")
    for _rd_key in $_rd_params $DECLARABLE_KEYS; do
        _rd_declared=$(declared "$1" "$_rd_key")
        if [ -n "$_rd_declared" ]; then
            _rd_value=${_rd_declared%%"$TAB"*}
            record '' '' "$_rd_key = ${_rd_value:-\"\"} (${_rd_declared#*"$TAB"})"
        elif is_word "$_rd_key" "$_rd_params"; then
            record '' '' "$_rd_key = \"\" (not declared)"
        fi
    done
}

# run_payload <id>
#
# In a subshell, so each unit sees its own parameters and no one else's.
run_payload() {
    (
        for _rp_param in $(unit_params "$1"); do
            _rp_declared=$(declared "$1" "$_rp_param")
            export "$(param_env_name "$_rp_param")=${_rp_declared%%"$TAB"*}"
        done
        exec sh -eu "$(unit_payload "$1")"
    )
}

DID=ran
[ "$SERVICES_DRY_RUN" -eq 1 ] && DID=plan

# The order file is read on its own descriptor so that a payload reading
# stdin cannot consume the units still to come.
while IFS= read -r id <&3; do
    [ "$(meta_field "$(unit_file "$id")" runs)" = "$SERVICES_RUN" ] || continue

    selection=$(unit_selection "$id")
    case "$selection" in
        skip:*)
            record "$id" skip "${selection#skip:}"
            continue
            ;;
        failed:*)
            record "$id" failed "${selection#failed:}"
            continue
            ;;
    esac

    if [ "$SERVICES_DRY_RUN" -eq 1 ]; then
        record "$id" "$DID" ''
        record_declared "$id"
        continue
    fi

    printf '[services] running %s\n' "$id"
    run_payload "$id" 3<&-
    status=$?
    if [ "$status" -ne 0 ]; then
        record "$id" failed "exit $status"
    elif [ -n "$(unit_setting "$id" every)" ] && ! schedule_record "$id"; then
        record "$id" failed 'ran, but its schedule could not be recorded'
    else
        record "$id" "$DID" ''
    fi
    record_declared "$id"
done 3< "$ORDER_IDS"

# ==============================================================================
# Report
# ==============================================================================

printf '\n[services] result\n'
awk -F'\t' '{
    line = sprintf("  %-18s %-7s %s", $1, $2, $3)
    sub(/ +$/, "", line)
    print line
}' "$RESULTS"

summarise() {
    awk -F'\t' -v want="$1" '$2 == want { n++ } END { print n + 0 }' "$RESULTS"
}
printf '\n[services] %s %s, %s skipped, %s failed\n' \
    "$(summarise "$DID")" "$DID" "$(summarise skip)" "$(summarise failed)"

if [ "$(summarise failed)" -ne 0 ]; then
    exit 1
fi
