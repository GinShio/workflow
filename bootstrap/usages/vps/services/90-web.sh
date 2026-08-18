#!/bin/sh
#@tags: usage:vps, scope:services, os:debian
# Services: Enable and start VPS web services

set -e

systemctl daemon-reload

# Nginx
if systemctl list-unit-files | grep -q nginx.service; then
    echo "Enabling Nginx..."
    systemctl enable --now nginx.service
fi
