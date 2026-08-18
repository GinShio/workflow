#!/bin/sh
#@tags: domain:cleanup, type:nightly, os:debian
set -u

sudo -A -- apt-get autoclean >/dev/null 2>&1
sudo -A -- apt-get autoremove -y >/dev/null 2>&1
