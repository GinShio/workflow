#!/bin/sh
#
# Running a unit's work with the privilege it asked for.
#
# There are no planes here. A run is one pass, by one user, covering both
# system and user work; a unit says `root: yes` when it needs privilege and
# the engine escalates for that unit alone. That is strictly less than a plane
# — no partitioning, no separate invocation, no separate state — and it is all
# the cases here need.
#
# The engine escalates and never de-escalates. Nothing today asks to be run as
# a named non-root user: a workstation run starts unprivileged and reaches up,
# a VPS run starts as root and has no user-level units at all. Should
# "configure this account from root" ever arrive, that is the second case, and
# the point to design for it rather than now.

# privilege_prepare
#
# Establishes that escalation will work, before any unit runs. Failing at the
# first `sudo` twenty minutes in wastes the twenty minutes; and a run whose
# selected units happen to need no privilege should not demand a passphrase at
# all.
privilege_prepare() {
    if [ "$(id -u)" -eq 0 ]; then
        BOOTSTRAP_ESCALATE=''
        return 0
    fi

    if [ "$1" -eq 0 ]; then
        BOOTSTRAP_ESCALATE=''
        return 0
    fi

    BOOTSTRAP_ESCALATE=yes

    # A dry run escalates nothing, so demanding the credentials would make the
    # one verb that is safe to run anywhere the one that needs the most setup.
    # Report the requirement instead.
    if [ "$BOOTSTRAP_DRY_RUN" -eq 1 ]; then
        printf '[bootstrap] %d unit(s) would need root via sudo\n' "$1"
        return 0
    fi

    command -v sudo >/dev/null 2>&1 || {
        printf 'Error: %d selected unit(s) need root and sudo is missing.\n' \
            "$1" >&2
        return 1
    }

    [ -n "${ROOT_PASSPHRASE:-}" ] || {
        printf 'Error: %d selected unit(s) need root; set ROOT_PASSPHRASE.\n' \
            "$1" >&2
        return 1
    }

    _pp_askpass="$BOOTSTRAP_SCRIPTS/get-root-passphrase.sh"
    [ -x "$_pp_askpass" ] || {
        printf 'Error: no executable askpass helper at %s\n' "$_pp_askpass" >&2
        return 1
    }
    export SUDO_ASKPASS="$_pp_askpass"

    sudo -A true || {
        printf 'Error: sudo askpass authentication failed.\n' >&2
        return 1
    }
}

# bootstrap_run <command>...
#
# The one way a unit's work reaches the system, so it is the one place that
# has to honour privilege and dry-run. Every installer path in `packages.sh`
# goes through it, which is what makes `apply -n` safe engine-wide rather than
# per-caller.
bootstrap_run() {
    if [ "$BOOTSTRAP_DRY_RUN" -eq 1 ]; then
        if [ "$BOOTSTRAP_UNIT_ROOT" = yes ]; then
            printf '      would run (root): %s\n' "$*"
        else
            printf '      would run: %s\n' "$*"
        fi
        return 0
    fi

    if [ "$BOOTSTRAP_UNIT_ROOT" = yes ] && [ -n "$BOOTSTRAP_ESCALATE" ]; then
        sudo -A -- "$@"
    else
        "$@"
    fi
}

# privilege_run_script <script>
#
# An action unit's payload. Invoked with `-eu` regardless of what the script
# itself sets: a unit is a contract, and a step that fails silently in the
# middle is the failure this design removes.
#
# `sudo -E` carries the ambient environment through, which is how a unit sees
# the secrets a run was given — DNS tokens, transcrypt passwords. HOME becomes
# root's, which is correct for root work; a unit needing root action against
# the invoking user's home escalates inside its own script, where it can say
# which paths it means.
privilege_run_script() {
    if [ "$BOOTSTRAP_DRY_RUN" -eq 1 ]; then
        if [ "$BOOTSTRAP_UNIT_ROOT" = yes ]; then
            printf '      would execute (root): %s\n' "$1"
        else
            printf '      would execute: %s\n' "$1"
        fi
        return 0
    fi

    if [ "$BOOTSTRAP_UNIT_ROOT" = yes ] && [ -n "$BOOTSTRAP_ESCALATE" ]; then
        sudo -AE -- sh -eu "$1"
    else
        sh -eu "$1"
    fi
}
