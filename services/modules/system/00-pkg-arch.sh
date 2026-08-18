#!/bin/sh
#@tags: domain:system, type:nightly, os:arch, schedule:5d
set -eu

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

cleanup() {
    sudo -k
    # shellcheck disable=SC1091
    . "$PROJECTS_SCRIPT_DIR/scripts/unproxy.sh"
}
trap cleanup EXIT

if command -v yay >/dev/null 2>&1; then
    # yay handles both repo and AUR updates
    yay -Syu --noconfirm
elif command -v paru >/dev/null 2>&1; then
    paru -Syu --noconfirm
else
    sudo -A -- pacman -Syu --noconfirm
fi
