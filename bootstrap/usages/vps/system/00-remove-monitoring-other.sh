#!/bin/sh
#@tags: usage:vps, scope:system, os:debian
# System: Remove other VPS provider monitoring tools

set -e

# Huawei Cloud
if [ -d "/usr/local/hostguard" ]; then
    echo "Removing Huawei Cloud monitoring tools..."
    systemctl stop hostguard 2>/dev/null || true
    systemctl disable hostguard 2>/dev/null || true
    [ -f /usr/local/hostguard/install/scripts/uninstall.sh ] && /usr/local/hostguard/install/scripts/uninstall.sh
    rm -rf /usr/local/hostguard
fi

# Baidu Cloud
if [ -d "/opt/bcm" ]; then
    echo "Removing Baidu Cloud monitoring tools..."
    systemctl stop bcm-agent 2>/dev/null || true
    systemctl disable bcm-agent 2>/dev/null || true
    [ -f /opt/bcm/uninstall.sh ] && /opt/bcm/uninstall.sh
    rm -rf /opt/bcm
fi

systemctl daemon-reload
