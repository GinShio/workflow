#!/bin/sh
#@tags: domain:user, type:nightly, dep:pipx, schedule:14d
set -eu

echo "Updating Pipx packages..."
pipx upgrade-all
