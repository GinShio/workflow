#!/bin/sh
#@tags: usage:vps, scope:system, os:debian, vps:oracle
# System: Remove Oracle Cloud monitoring tools

set -e

echo "Removing Oracle Cloud monitoring tools..."

if systemctl is-active --quiet oracle-cloud-agent || systemctl is-enabled --quiet oracle-cloud-agent 2>/dev/null; then
    systemctl stop oracle-cloud-agent 2>/dev/null || true
    systemctl disable oracle-cloud-agent 2>/dev/null || true
    apt-get purge -y oracle-cloud-agent 2>/dev/null || true
fi

systemctl daemon-reload
echo "Oracle Cloud monitoring tools removed successfully."
