#!/bin/sh
#@tags: usage:vps, scope:system, os:debian, vps:aws
# System: Remove AWS monitoring tools

set -e

echo "Removing AWS monitoring tools..."

if systemctl is-active --quiet amazon-ssm-agent || systemctl is-enabled --quiet amazon-ssm-agent 2>/dev/null; then
    systemctl stop amazon-ssm-agent 2>/dev/null || true
    systemctl disable amazon-ssm-agent 2>/dev/null || true
    apt-get purge -y amazon-ssm-agent 2>/dev/null || true
fi

systemctl daemon-reload
echo "AWS monitoring tools removed successfully."
