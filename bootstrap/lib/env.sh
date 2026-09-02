#!/bin/sh
#
# What a run has to be *given*: the ambient environment each unit reads.
#
# Declared at `units/<id>/env`, one `NAME: description` per line, read by the
# same `key: value` parser as everything else here. Three shapes, in the same
# spirit as the three a `packages` file has:
#
#   VPS_DOMAIN_NAME              required — the run refuses to start
#   DNS_PROVIDER?                optional — its absence changes the outcome
#   WITS_TRANSCRYPT_*_PASSWORD   a family the unit discovers for itself
#
# Only a fixed name can be checked, exactly as only an exact package selector
# can be diffed against the installed set. A name containing `*` is printed so
# the contract is complete and never checked: which members of it a run needs
# is the unit's own question, and `dotfiles` answers it by reading the
# environment itself.
#
# This is the opposite direction from `BOOTSTRAP_*`. Those the engine computes
# and exports, and the README states them once for every unit; an `env` file
# says what the *caller* has to supply, beside the unit that reads it. A name
# documented in prose somewhere else is a name nobody is told about at the one
# moment it matters.
#
# The gate exists for the reason `privilege_prepare` exists: a run that dies
# on a missing DNS token twenty minutes in wasted the twenty minutes.

# env_file <id>
#
# Prints nothing when the unit declares no environment, which most do not.
env_file() {
    _ef_path="$BOOTSTRAP_UNITS/$1/env"
    if [ -f "$_ef_path" ]; then
        printf '%s\n' "$_ef_path"
    fi
}

# env_validate <id>
#
# Rejects a declaration that cannot mean what it says, for the reason
# `unit_validate` rejects an unknown metadata key: a line the parser drops is
# a name nobody checks and nobody prints, which is the silent skip this design
# exists to remove. A missing `:` is the way to write one by accident.
env_validate() {
    _ev_file=$(env_file "$1")
    [ -n "$_ev_file" ] || return 0

    _ev_bad=$(awk -v id="$1" '
        /^[ \t]*#/ { next }
        /^[ \t]*$/ { next }
        index($0, ":") == 0 {
            printf "unit %s: env line %d has no `:` — `%s`\n", id, FNR, $0
            next
        }
        {
            k = substr($0, 1, index($0, ":") - 1)
            gsub(/^[ \t]+|[ \t]+$/, "", k)
            if (k !~ /^[A-Za-z_*][A-Za-z0-9_*]*[?]?$/) {
                printf "unit %s: env name `%s` is not one a shell can export\n", id, k
            }
        }
    ' "$_ev_file")

    [ -z "$_ev_bad" ] || {
        printf '%s\n' "$_ev_bad" >&2
        return 1
    }
}

# env_table <selected-ids-file>
#
# One row per name declared by a unit this run will actually reach:
#
#   <unit> \t <name> \t required|optional|family \t set|unset|- \t <description>
#
# Gathered in one pass and reported from the table below, so the gate's
# refusal and the dry run's listing cannot disagree about what a run reads.
#
# A unit the engine will report `cached` is not asked for its inputs: its work
# is recorded as done, and demanding the token that produced it would make an
# idempotent re-run harder than the first run was.
#
# `unless` and `optional` are deliberately not consulted, because both can
# change within a run and this is decided before it starts. So a *required*
# name on a unit whose `unless` reads the environment would be demanded even
# where that unit is already satisfied — declare such a name optional and
# keep the relation in the script, the way `certbot-dns` keeps "a token is
# needed once a provider is named".
env_table() {
    # Globbing off for the same reason a selector file is read with it off: a
    # declared name is data, and `WITS_TRANSCRYPT_*_PASSWORD` must not be
    # expanded against whatever directory the run was started from.
    set -f
    while IFS= read -r _et_id || [ -n "$_et_id" ]; do
        _et_file=$(env_file "$_et_id")
        [ -n "$_et_file" ] || continue
        state_covers "$_et_id" && continue

        for _et_key in $(meta_keys "$_et_file"); do
            case "$_et_key" in
                *'*'*) _et_shape=family ;;
                *'?')  _et_shape=optional ;;
                *)     _et_shape=required ;;
            esac

            # The environment, not the shell: a unit's script is a child
            # process, so an unexported variable is one it cannot see. An
            # empty value counts as unset, which is what every script here
            # already tests for with `${NAME:-}`.
            _et_name=${_et_key%\?}
            if [ "$_et_shape" = family ]; then
                _et_held='-'
            elif [ -n "$(printenv "$_et_name" 2>/dev/null)" ]; then
                _et_held=set
            else
                _et_held=unset
            fi

            printf '%s\t%s\t%s\t%s\t%s\n' \
                "$_et_id" "$_et_name" "$_et_shape" "$_et_held" \
                "$(meta_field "$_et_file" "$_et_key")"
        done
    done < "$1"
    set +f
}

# env_prepare <selected-ids-file>
#
# Establishes the run's inputs before any unit runs. Every missing name is
# reported at once: fixing one and rediscovering the next is the same waste,
# serialised.
#
# A dry run reports the whole contract and refuses nothing, for the reason
# `privilege_prepare` does not demand a passphrase under `-n` — the one verb
# that is safe to run anywhere should not be the one that needs the most
# setting up.
env_prepare() {
    _ep_table="$BOOTSTRAP_TMP/env"
    env_table "$1" > "$_ep_table"
    [ -s "$_ep_table" ] || return 0

    if [ "$BOOTSTRAP_DRY_RUN" -eq 1 ]; then
        printf '[bootstrap] environment this run reads\n'
        awk -F'\t' '!seen[$2]++ { printf "  %-6s %-9s %-27s %s\n", $4, $3, $2, $5 }' \
            "$_ep_table"
        printf '\n'
        return 0
    fi

    _ep_lacking=$(awk -F'\t' '$3 == "required" && $4 == "unset"' "$_ep_table")
    if [ -n "$_ep_lacking" ]; then
        printf 'Error: %d required environment variable(s) are not set.\n' \
            "$(printf '%s\n' "$_ep_lacking" | awk 'END { print NR }')" >&2
        printf '%s\n' "$_ep_lacking" |
            awk -F'\t' '{ printf "  %-27s %-14s %s\n", $2, $1, $5 }' >&2
        return 1
    fi

    # Not a failure, but the difference between an apex certificate and a
    # wildcard is worth saying before the work rather than after it. What the
    # absence means belongs to the name, so the header only reports it and
    # each description does the explaining.
    _ep_absent=$(awk -F'\t' '$3 == "optional" && $4 == "unset" && !seen[$2]++ {
            printf "  %-27s %s\n", $2, $5
        }' "$_ep_table")
    if [ -n "$_ep_absent" ]; then
        printf '[bootstrap] optional and unset:\n'
        printf '%s\n\n' "$_ep_absent"
    fi
}
