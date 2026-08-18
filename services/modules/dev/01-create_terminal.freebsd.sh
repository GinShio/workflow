#!/bin/sh
#@tags: domain:dev, type:autostart, dep:tmux, os:freebsd
set -eu

_work_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/runner"
mkdir -p "$_work_dir"

# FreeBSD: Use jail for resource limitation
# Requires sudo privileges to create jail
# Uses path=/ to share filesystem (thin jail) but isolates processes

_default_shell=$(tmux -c 'echo $SHELL')

WRAPPER_CMD="sudo -A jail -c name=tmux-runner path=/ host=inherit ip4=inherit exec.start=\"/usr/bin/env sh\" command=/usr/bin/su -m $(id -un) -c $_default_shell"

tmux new-session -d -s runner -c "$_work_dir" $WRAPPER_CMD

# Start Editor Session
tmux new-session -d -s editor -c "$HOME"
tmux send-keys -t editor "emacsclient -nw --eval '(doom/load-session \"${XDG_CONFIG_HOME:-$HOME/.config}/emacs/.local/etc/workspaces/projs\")'" ENTER
