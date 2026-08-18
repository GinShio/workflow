#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:node
set -u

if command -v npm >/dev/null 2>&1; then
    npm cache clean --force >/dev/null 2>&1 || true
fi
if command -v yarn >/dev/null 2>&1; then
    yarn cache clean >/dev/null 2>&1 || true
fi
