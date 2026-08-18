#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:docker
set -u

if docker info >/dev/null 2>&1; then
    docker system prune -f --volumes >/dev/null 2>&1
elif sudo -n true 2>/dev/null; then
    sudo -A -- docker system prune -f --volumes >/dev/null 2>&1
fi
