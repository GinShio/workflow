#!/bin/sh
#@tags: domain:cleanup, type:nightly, os:arch
set -u

# 1. Cache
if command -v paccache >/dev/null 2>&1; then
    sudo -A -- paccache -rk2 >/dev/null 2>&1
    sudo -A -- paccache -ruk0 >/dev/null 2>&1
else
    sudo -A -- pacman -Sc --noconfirm >/dev/null 2>&1
fi

# 2. Orphans (Recursive)
_orphans=$(pacman -Qtdq 2>/dev/null || true)
if [ -n "$_orphans" ]; then
    # shellcheck disable=SC2086
    sudo -A -- pacman -Rns --noconfirm $_orphans >/dev/null 2>&1 || true
fi

# 3. AUR Cache
if command -v yay >/dev/null 2>&1; then
    yay -Sc --noconfirm >/dev/null 2>&1 || true
    yay -Yc --noconfirm >/dev/null 2>&1 || true
elif command -v paru >/dev/null 2>&1; then
    paru -Sc --noconfirm >/dev/null 2>&1 || true
    paru -c --noconfirm >/dev/null 2>&1 || true
fi
