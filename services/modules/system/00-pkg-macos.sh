#!/bin/sh
#@tags: domain:system, type:nightly, os:darwin, schedule:5d
set -eu

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

cleanup() {
    # shellcheck disable=SC1091
    . "$PROJECTS_SCRIPT_DIR/scripts/unproxy.sh"
}
trap cleanup EXIT

if command -v brew >/dev/null 2>&1; then
    brew update
    brew upgrade
fi
