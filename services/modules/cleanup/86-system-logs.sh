#!/bin/sh
#@tags: domain:cleanup, type:nightly
set -u

if command -v journalctl >/dev/null 2>&1; then
    sudo -A -- journalctl --vacuum-time=2weeks >/dev/null 2>&1
fi
if command -v systemd-tmpfiles >/dev/null 2>&1; then
    sudo -A -- systemd-tmpfiles --clean >/dev/null 2>&1
else
    # Fallback for non-systemd Linux & FreeBSD
    find /tmp -depth -type f -atime +10 -delete 2>/dev/null || true
    
    # Also clean /var/tmp which is often persistent
    find /var/tmp -depth -type f -atime +30 -delete 2>/dev/null || true
fi

# Traditional /var/log (Linux & FreeBSD)
# Cleans old rotated/compressed logs older than 30 days.
if [ -d "/var/log" ]; then
    sudo -A -- find /var/log -type f \( \
        -name "*.gz" -o \
        -name "*.xz" -o \
        -name "*.bz2" -o \
        -name "*.Z" -o \
        -name "*.zip" -o \
        -name "*.old" \
    \) -mtime +30 -delete 2>/dev/null || true
fi

# macOS Logs
if [ "$(uname -s)" = "Darwin" ]; then
    sudo -A -- rm -rf /private/var/log/asl/*.asl 2>/dev/null || true
    find "$HOME/Library/Logs" -type f -mtime +14 -delete 2>/dev/null || true
fi
