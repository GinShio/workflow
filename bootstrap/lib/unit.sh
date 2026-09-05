#!/bin/sh
#
# Unit metadata: reading it, and deciding whether a unit applies here.
#
# A unit is a directory under `units/` holding a `unit` metadata file and a
# payload. Two kinds, distinguished by what the payload is:
#
#   kind: packages   payload is `packages[.<platform>]` — one name per line
#   kind: action     payload is `run[.<platform>]` — a POSIX shell script
#
# The split between `capabilities` and `when` is the one distinction the whole
# selection model rests on: a capability is a *choice* a machine's registry
# entry declares, a `when` fact is something detected about the hardware. A
# desktop cannot be detected before it is installed, so it must be declared;
# a GPU cannot be declared into existence, so it must be detected.

UNIT_KEYS='kind manager capabilities requires when optional root unless'

# unit_field <id> <key>
unit_field() {
    meta_field "$BOOTSTRAP_UNITS/$1/unit" "$2"
}

# unit_list <id> <key>
unit_list() {
    meta_list "$BOOTSTRAP_UNITS/$1/unit" "$2"
}

# unit_validate <id>
#
# Rejects a unit whose metadata cannot mean what it says. Called for every
# listed unit before anything runs, because each of these failures would
# otherwise present as a silent skip — the failure mode this design exists to
# remove.
unit_validate() {
    _uv_file="$BOOTSTRAP_UNITS/$1/unit"

    if [ ! -f "$_uv_file" ]; then
        printf 'unit %s: no `unit` file at %s\n' "$1" "$_uv_file" >&2
        return 1
    fi

    _uv_bad=0
    for _uv_key in $(meta_keys "$_uv_file"); do
        case " $UNIT_KEYS " in
            *" $_uv_key "*) ;;
            *)
                printf 'unit %s: unknown key `%s`\n' "$1" "$_uv_key" >&2
                _uv_bad=1
                ;;
        esac
    done

    case "$(unit_field "$1" kind)" in
        packages|action) ;;
        '')
            printf 'unit %s: missing `kind`\n' "$1" >&2
            _uv_bad=1
            ;;
        *)
            printf 'unit %s: unknown kind `%s`\n' \
                "$1" "$(unit_field "$1" kind)" >&2
            _uv_bad=1
            ;;
    esac

    case "$(unit_field "$1" root)" in
        ''|yes) ;;
        *)
            printf 'unit %s: `root` takes `yes` or nothing, not `%s`\n' \
                "$1" "$(unit_field "$1" root)" >&2
            _uv_bad=1
            ;;
    esac

    # A unit with no payload for *any* platform is dead weight: it would read
    # as a platform skip on every machine and never say it was a mistake.
    #
    # Tested one path at a time because an unmatched glob stays literal, and
    # the `-f` on that literal is what turns "no platform variants" into a
    # clean negative.
    _uv_kind=$(unit_field "$1" kind)
    _uv_base=run
    [ "$_uv_kind" = packages ] && _uv_base=packages
    _uv_found=0
    for _uv_path in "$BOOTSTRAP_UNITS/$1/$_uv_base" \
                    "$BOOTSTRAP_UNITS/$1/$_uv_base".*; do
        if [ -f "$_uv_path" ]; then
            _uv_found=1
            break
        fi
    done
    if [ "$_uv_found" -eq 0 ]; then
        printf 'unit %s: kind `%s` needs a `%s` payload, none found\n' \
            "$1" "$_uv_kind" "$_uv_base" >&2
        _uv_bad=1
    fi

    return "$_uv_bad"
}

# unit_payload <id>
#
# The payload file this machine should use, or nothing when the unit has none
# for it.
#
# Two suffixes may name a platform, and they name two independent detected
# facts rather than a chain: `.<distro>` (opensuse, debian) and `.<os>`
# (linux, freebsd). A unit carrying both answers the specific question and the
# general one, so the distro file wins. There is no inheritance between
# distros — a derivative gets its own file and gets nothing for free, which is
# what keeps a package list authoritative on its own terms.
unit_payload() {
    _up_dir="$BOOTSTRAP_UNITS/$1"
    case "$(unit_field "$1" kind)" in
        packages) _up_base="$_up_dir/packages" ;;
        action)   _up_base="$_up_dir/run" ;;
        *)        return 0 ;;
    esac

    if [ -n "$BOOTSTRAP_DISTRO" ] && [ -f "$_up_base.$BOOTSTRAP_DISTRO" ]; then
        printf '%s\n' "$_up_base.$BOOTSTRAP_DISTRO"
    elif [ -f "$_up_base.$BOOTSTRAP_OS" ]; then
        printf '%s\n' "$_up_base.$BOOTSTRAP_OS"
    elif [ -f "$_up_base" ]; then
        printf '%s\n' "$_up_base"
    fi
}

# unit_fact_holds <fact>
#
# One `when` item. Exit 0 when it holds, 1 when it does not, 2 when the
# vocabulary does not contain it — a typo has to fail the run rather than read
# as an unmet condition and silently drop the unit.
#
# The vocabulary is deliberately small: facts about hardware and about the
# virtualisation platform. `os:` is absent because a payload suffix already
# says which platform a unit is for, and desktop-environment facts are absent
# because a desktop is a choice the registry declares, not a property of a
# machine that has not been set up yet.
unit_fact_holds() {
    case "$1" in
        gpu:any)
            [ -n "$BOOTSTRAP_GPUS" ]
            ;;
        gpu:*)
            case " $BOOTSTRAP_GPUS " in
                *" ${1#gpu:} "*) return 0 ;;
                *) return 1 ;;
            esac
            ;;
        cpu:*)
            [ "${1#cpu:}" = "$BOOTSTRAP_CPU" ]
            ;;
        hw:laptop)
            [ "$BOOTSTRAP_LAPTOP" -eq 1 ]
            ;;
        vps:*)
            # Detection costs a DMI read and possibly `systemd-detect-virt`,
            # so it waits until a unit actually asks.
            if [ -z "${BOOTSTRAP_VPS:-}" ]; then
                # shellcheck source=../../scripts/detect_vps.sh
                . "$BOOTSTRAP_SCRIPTS/detect_vps.sh"
                BOOTSTRAP_VPS=$(detect_vps)
            fi
            [ "${1#vps:}" = "$BOOTSTRAP_VPS" ]
            ;;
        *)
            return 2
            ;;
    esac
}

# unit_selection <id>
#
# Prints `selected`, or `skip:<reason>` naming the first condition that failed.
# Exit 2 on a fact this vocabulary does not contain.
#
# Only conditions that cannot change during a run are decided here:
# capabilities, hardware facts, and whether a payload exists for this
# platform. `optional` is not among them — see `unit_available`.
#
# Every skip carries its reason, and the runner prints all of them. An
# unexplained absence is indistinguishable from a bug.
unit_selection() {
    _us_id=$1

    _us_caps=$(unit_list "$_us_id" capabilities)
    if [ -n "$_us_caps" ]; then
        _us_hit=0
        for _us_cap in $_us_caps; do
            case " $BOOTSTRAP_CAPABILITIES " in
                *" $_us_cap "*) _us_hit=1; break ;;
            esac
        done
        if [ "$_us_hit" -eq 0 ]; then
            printf 'skip:needs capability %s\n' \
                "$(printf '%s' "$_us_caps" | tr '\n' '|')"
            return 0
        fi
    fi

    for _us_fact in $(unit_list "$_us_id" when); do
        unit_fact_holds "$_us_fact"
        case $? in
            0) ;;
            1)
                printf 'skip:%s does not hold\n' "$_us_fact"
                return 0
                ;;
            *)
                printf 'unit %s: unknown fact `%s`\n' "$_us_id" "$_us_fact" >&2
                return 2
                ;;
        esac
    done

    if [ -z "$(unit_payload "$_us_id")" ]; then
        printf 'skip:no payload for %s\n' "${BOOTSTRAP_DISTRO:-$BOOTSTRAP_OS}"
        return 0
    fi

    printf 'selected\n'
}

# unit_available <id>
#
# Whether the subsystem this unit needs is present. Prints nothing and exits 0
# when it is, or the reason it is not.
#
# Asked immediately before the unit runs, not during selection, because the
# answer changes *within* a run: `pnpm-apps` needs a `pnpm` that
# `develop-packages` installs a few units earlier. Deciding it upfront would
# skip every unit whose tool this run is about to provide — which is what the
# tag runner's `dep:` avoided by testing lazily too.
unit_available() {
    for _ua_cmd in $(unit_list "$1" optional); do
        if ! command -v "$_ua_cmd" >/dev/null 2>&1; then
            printf '%s not present\n' "$_ua_cmd"
            return 1
        fi
    done
}

# unit_satisfied <id>
#
# Whether the machine already shows what this unit would do. Exit 0 when it
# does.
#
# For `kind: packages` the answer is exact and free — the installed set
# already had to be read to know what to install — so the runner asks
# `packages_missing` instead and this only handles a declared `unless`.
unit_satisfied() {
    _usa_probe=$(unit_field "$1" unless)
    [ -n "$_usa_probe" ] || return 1
    sh -c "$_usa_probe" >/dev/null 2>&1
}
