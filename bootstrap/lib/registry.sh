#!/bin/sh
#
# The machine registry: one file per target under `registry/`.
#
# A target answers "which machine is this", and the answer is given on the
# command line rather than detected. Detecting it is circular — a machine that
# has never been bootstrapped carries whatever hostname its installer chose,
# and setting the hostname is one of the things bootstrap does. So the target
# names the entry, and the entry says what the machine should become.
#
# One file per machine rather than one table: `git blame` then points at a
# single machine, adding one is adding a file, and listing them is `ls`.

REGISTRY_KEYS='hostname capabilities profile'

# registry_file <target>
registry_file() {
    printf '%s\n' "$BOOTSTRAP_REGISTRY/$1"
}

# registry_targets
registry_targets() {
    [ -d "$BOOTSTRAP_REGISTRY" ] || return 0
    for _rt_file in "$BOOTSTRAP_REGISTRY"/*; do
        [ -f "$_rt_file" ] || continue
        basename "$_rt_file"
    done
}

# registry_load <target>
#
# Sets BOOTSTRAP_CAPABILITIES (space separated), BOOTSTRAP_HOSTNAME and
# BOOTSTRAP_PROFILE. Fails with the list of known targets, because the useful
# thing to say to someone who named the wrong one is what the right ones are.
registry_load() {
    _rl_file="$BOOTSTRAP_REGISTRY/$1"

    if [ ! -f "$_rl_file" ]; then
        printf 'No registry entry `%s`.\n' "$1" >&2
        printf 'Known targets:\n' >&2
        registry_targets | sed 's/^/  /' >&2
        printf 'Or name capabilities directly with --capabilities.\n' >&2
        return 1
    fi

    _rl_bad=0
    for _rl_key in $(meta_keys "$_rl_file"); do
        case " $REGISTRY_KEYS " in
            *" $_rl_key "*) ;;
            *)
                printf '%s: unknown key `%s`\n' "$_rl_file" "$_rl_key" >&2
                _rl_bad=1
                ;;
        esac
    done
    [ "$_rl_bad" -eq 0 ] || return 1

    BOOTSTRAP_CAPABILITIES=$(meta_list "$_rl_file" capabilities | tr '\n' ' ')
    BOOTSTRAP_CAPABILITIES=${BOOTSTRAP_CAPABILITIES% }
    BOOTSTRAP_HOSTNAME=$(meta_field "$_rl_file" hostname)
    BOOTSTRAP_PROFILE=$(meta_field "$_rl_file" profile)

    if [ -z "$BOOTSTRAP_CAPABILITIES" ]; then
        printf '%s: declares no capabilities, so it would install nothing.\n' \
            "$_rl_file" >&2
        return 1
    fi
}

# registry_capabilities_declared
#
# Every capability any entry names, for rejecting a unit that asks for one
# nothing can ever declare. A unit gated on a misspelled capability is
# invisible on every machine, which is the silent-skip failure again.
registry_capabilities_declared() {
    [ -d "$BOOTSTRAP_REGISTRY" ] || return 0
    for _rcd_file in "$BOOTSTRAP_REGISTRY"/*; do
        [ -f "$_rcd_file" ] || continue
        meta_list "$_rcd_file" capabilities
    done | sort -u
}
