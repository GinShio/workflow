#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:npm
set -u

# `npm cache verify` only garbage-collects entries the index no longer
# references, which on a content-addressed store reclaims almost nothing. This
# module exists to return disk, so it empties the cache; npm refetches.
npm cache clean --force >/dev/null 2>&1 || true
