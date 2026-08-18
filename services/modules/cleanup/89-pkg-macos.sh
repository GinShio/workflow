#!/bin/sh
#@tags: domain:cleanup, type:nightly, os:darwin
set -u

if command -v brew >/dev/null 2>&1; then
    brew cleanup -s >/dev/null 2>&1
fi
