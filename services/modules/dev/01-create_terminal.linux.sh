#!/bin/sh
#@tags: domain:dev, type:autostart, os:linux, dep:tmux, dep:systemd-run, dep:emacsclient
set -eu

. "$PROJECTS_SCRIPT_DIR/scripts/detect.sh"
mem_total=$(detect_memory_mb)

_work_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/runner"
mkdir -p "$_work_dir"

# Calculate MemoryMax: 75% of RAM, min 4G
# Shell integer math: (mem_mb / 1024) * 3 / 4
_mem_gb=$(( mem_total / 1024 ))
_target_gb=$(( _mem_gb * 3 / 4 ))
if [ "$_target_gb" -lt 4 ]; then _target_gb=4; fi

if ! tmux has-session -t runner 2>/dev/null; then
    tmux new-session -d -s runner -c "$_work_dir" \
        systemd-run --user --scope \
        -p MemoryMax=${_target_gb}G \
        -p MemorySwapMax=0 \
        -p TasksMax=512 \
        -p OOMPolicy=continue \
        --unit=tmux-runner \
        --shell
fi
# systemctl --user show tmux-runner.scope -p MemoryCurrent -p MemoryMax

# `-a ''` starts the daemon when none is listening; without it emacsclient
# exits at once on a cold boot and takes the freshly created session with it.
if ! tmux has-session -t editor 2>/dev/null; then
    tmux new-session -d -s editor -c "$HOME" 'emacsc -nw'
fi
