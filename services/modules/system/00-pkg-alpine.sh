#!/bin/sh
#@tags: domain:system, type:nightly, os:alpine, schedule:5d
set -eu

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

cleanup() {
    sudo -k
    # shellcheck disable=SC1091
    . "$PROJECTS_SCRIPT_DIR/scripts/unproxy.sh"
}
trap cleanup EXIT

sudo -A -- apk update
sudo -A -- apk upgrade
