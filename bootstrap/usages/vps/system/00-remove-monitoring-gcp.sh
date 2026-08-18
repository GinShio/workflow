#!/bin/sh
#@tags: usage:vps, scope:system, os:debian, vps:gcp
# System: Remove Google Cloud monitoring tools

set -e

echo "Removing Google Cloud monitoring tools..."

if systemctl is-active --quiet google-osconfig-agent || systemctl is-enabled --quiet google-osconfig-agent 2>/dev/null; then
    systemctl stop google-osconfig-agent 2>/dev/null || true
    systemctl disable google-osconfig-agent 2>/dev/null || true
    apt-get purge -y google-osconfig-agent 2>/dev/null || true
fi

systemctl daemon-reload
echo "Google Cloud monitoring tools removed successfully."
