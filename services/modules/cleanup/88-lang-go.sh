#!/bin/sh
#@tags: domain:cleanup, type:nightly, dep:go
set -u

go clean -modcache >/dev/null 2>&1 || true
go clean -cache >/dev/null 2>&1 || true
