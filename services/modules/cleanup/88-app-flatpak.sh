#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:flatpak
set -u

flatpak uninstall --unused -y >/dev/null 2>&1 || true
