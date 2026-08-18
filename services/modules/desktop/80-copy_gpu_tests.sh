#!/bin/sh
#@tags: domain:desktop, type:autostart, gpu:any, dep:python3
set -eu

# shellcheck disable=SC1091
python3 "$PROJECTS_SCRIPT_DIR/gputest.py" install
python3 "$PROJECTS_SCRIPT_DIR/gputest.py" restore --days 10
