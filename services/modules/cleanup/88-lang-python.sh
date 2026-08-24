#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:python3
set -u

# Empties ~/.cache/pip: built wheels and cached HTTP responses, all of which
# pip re-creates on demand. Invoked through python3 because a bare `pip` is
# routinely absent on distributions that still ship python3.
python3 -m pip cache purge >/dev/null 2>&1 || true
