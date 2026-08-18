#!/bin/sh
#@tags: domain:user, type:nightly, dep:flatpak, schedule:14d
set -eu

echo "Updating Flatpak packages..."
flatpak update -y
