#!/bin/sh
#@tags: domain:system, type:nightly, os:fedora, schedule:5d
set -eu

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

cleanup() {
    sudo -k
    # shellcheck disable=SC1091
    . "$PROJECTS_SCRIPT_DIR/scripts/unproxy.sh"
}
trap cleanup EXIT

sudo -AE -- dnf upgrade -y --refresh
