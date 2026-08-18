#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:podman
set -u

if podman info >/dev/null 2>&1; then
    podman system prune -f --volumes >/dev/null 2>&1
fi
