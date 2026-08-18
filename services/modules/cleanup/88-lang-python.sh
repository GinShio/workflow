#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:python3
set -u

pip cache purge >/dev/null 2>&1 || true
