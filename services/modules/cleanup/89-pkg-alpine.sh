#!/bin/sh
#@tags: domain:cleanup, type:nightly, os:alpine
set -u

sudo -A -- apk cache clean >/dev/null 2>&1
