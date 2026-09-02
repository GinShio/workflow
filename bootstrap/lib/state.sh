#!/bin/sh
#
# What this machine has already had done to it.
#
# State is a *fallback*, never the primary truth. A unit that can ask the
# machine — every `kind: packages` unit, and every action carrying an
# `unless` — is asked, because a record can be stale, can be lost, and cannot
# see a change somebody made by hand. State covers only what is undecidable:
# `zypper dup` has no "already done", a meson build has no cheap probe.
#
# So its purpose is not correctness. Nearly every unit here is idempotent
# already, which is why re-running a broken bootstrap has always happened to
# work. Its purpose is to make re-entry *cheap*: a forty-minute run that died
# at ninety percent should cost four minutes the second time, not forty.
#
# One location, owned by whoever invoked bootstrap. There is no per-privilege
# split because there is no plane split: a run escalates for individual units
# but is one run, by one user, and that user owns the record.

# state_dir
state_dir() {
    printf '%s/wits/bootstrap\n' "${XDG_STATE_HOME:-$HOME/.local/state}"
}

# state_hash
#
# Reads stdin, prints a digest. The ladder exists because no one hashing tool
# is present on every system bootstrap has to run on before it has installed
# anything.
state_hash() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{print $1}'
    elif command -v sha256 >/dev/null 2>&1; then
        sha256 -q
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 | awk '{print $NF}'
    else
        cksum | awk '{print $1 "-" $2}'
    fi
}

# state_digest <id>
#
# Identifies the unit's *content*, so that editing metadata or a payload
# invalidates the record and the unit runs again. Paths are sorted so repeated
# runs agree.
state_digest() {
    find "$BOOTSTRAP_UNITS/$1" -type f | sort | {
        while IFS= read -r _sd_file || [ -n "$_sd_file" ]; do
            printf '%s\n' "$_sd_file"
            cat "$_sd_file"
        done
    } | state_hash
}

# state_done <id>
#
# Exit 0 when this exact content has been recorded as succeeding here.
state_done() {
    _sd_record="$(state_dir)/$1"
    [ -f "$_sd_record" ] || return 1
    [ "$(cat "$_sd_record")" = "$(state_digest "$1")" ]
}

# state_covers <id>
#
# Exit 0 when the record is the whole answer for this unit: it carries no
# probe of its own, and a record matches its content. The runner reports such
# a unit `cached` and does no work, so the two callers that have to agree on
# what "will not run" means — the runner and the environment gate, which must
# not demand inputs for work that is already done — ask this one question.
state_covers() {
    [ -z "$(unit_field "$1" unless)" ] && state_done "$1"
}

# state_record <id>
#
# Written through a temporary file: a run interrupted mid-write must not leave
# a truncated digest that would read as a mismatch forever.
state_record() {
    _sr_dir=$(state_dir)
    mkdir -p "$_sr_dir" || return 1
    _sr_tmp=$(mktemp "$_sr_dir/.record.XXXXXX") || return 1
    if state_digest "$1" > "$_sr_tmp" && mv "$_sr_tmp" "$_sr_dir/$1"; then
        return 0
    fi
    rm -f "$_sr_tmp"
    return 1
}

# state_forget <id>
state_forget() {
    rm -f "$(state_dir)/$1"
}
