#!/bin/sh
#@tags: domain:cleanup, type:nightly, os:freebsd
set -u

sudo -A -- pkg clean -y >/dev/null 2>&1
sudo -A -- pkg autoremove -y >/dev/null 2>&1
