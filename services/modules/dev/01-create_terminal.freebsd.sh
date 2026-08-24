#!/bin/sh
#@tags: domain:dev, type:autostart, dep:tmux, dep:sudo, dep:jail, dep:emacsclient, os:freebsd
set -eu

_work_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/runner"
mkdir -p "$_work_dir"

# FreeBSD: Use jail for resource limitation
# Requires sudo privileges to create jail
# Uses path=/ to share filesystem (thin jail) but isolates processes

_default_shell=${SHELL:-/bin/sh}
_user=$(id -un)

# jail(8): `command` is a synonym for `exec.start`, and an exec.* parameter
# given twice runs both values in sequence. Only one of the two may appear, or
# jail creation blocks on an interactive `sh` before reaching the login shell.
# The value is an sh(1) command line, so it stays a single argument.
if ! tmux has-session -t runner 2>/dev/null; then
    tmux new-session -d -s runner -c "$_work_dir" \
        sudo -A jail -c \
        name=tmux-runner \
        path=/ \
        host=inherit \
        ip4=inherit \
        "command=/usr/bin/su -m $_user -c $_default_shell"
fi

# `-a ''` starts the daemon when none is listening; without it emacsclient
# exits at once on a cold boot and takes the freshly created session with it.
if ! tmux has-session -t editor 2>/dev/null; then
    tmux new-session -d -s editor -c "$HOME" emacsclient -nw -a ''
fi
