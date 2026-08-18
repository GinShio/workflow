#!/bin/sh
#@tags: usage:dev, scope:services, dep:systemctl
# Services: User Services

echo "Enabling User Services..."
USER_SERVICES="
    nightly-script.timer
    emacs.service
    develop-autostart.service
    podman.service
    aria2.service
    qbittorrent-nox.service
"

for svc in $USER_SERVICES; do
    # Check user unit existence roughly or just try enable
    systemctl --user enable --now "$svc" || echo "Note: User service $svc could not be enabled (missing?)"
done
