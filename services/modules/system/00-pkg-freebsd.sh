#!/bin/sh
#@tags: domain:system, type:nightly, os:freebsd, schedule:5d
set -eu

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

cleanup() {
    sudo -k
    # shellcheck disable=SC1091
    . "$PROJECTS_SCRIPT_DIR/scripts/unproxy.sh"
}
trap cleanup EXIT

sudo -AE -- pkg update
sudo -AE -- pkg upgrade -y
