#!/bin/sh
#@tags: domain:cleanup, type:nightly, os:opensuse, dep:zypper
set -u

sudo -A -- zypper clean --all >/dev/null 2>&1
