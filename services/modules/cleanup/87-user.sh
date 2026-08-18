#!/bin/sh
#@tags: domain:cleanup, type:nightly, scope:user
set -u

_cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}"
_data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"

if [ -d "$_cache_dir" ]; then
    find "$_cache_dir" -depth -type f -atime +60 -delete 2>/dev/null || true
fi
if [ -d "$_cache_dir/thumbnails" ]; then
    find "$_cache_dir/thumbnails" -type f -atime +14 -delete 2>/dev/null || true
fi
if [ -d "$_data_dir/Trash/files" ]; then
    find "$_data_dir/Trash/files" -depth -type f -atime +30 -delete 2>/dev/null || true
    find "$_data_dir/Trash/info" -depth -type f -atime +30 -delete 2>/dev/null || true
fi
if [ -f "$HOME/.xsession-errors" ]; then
    _size=$(du -m "$HOME/.xsession-errors" | cut -f1)
    if [ "$_size" -gt 50 ]; then
        : > "$HOME/.xsession-errors"
    fi
fi
