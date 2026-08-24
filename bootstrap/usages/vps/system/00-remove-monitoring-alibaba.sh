#!/bin/sh
#@tags: usage:vps, scope:system, os:debian, vps:alibaba
# System: Remove Alibaba Cloud monitoring tools

set -e

echo "Removing Alibaba Cloud monitoring tools..."

if [ -d "/usr/local/aegis" ] || [ -d "/usr/local/cloudmonitor" ]; then
    systemctl stop aliyun.service 2>/dev/null || true
    systemctl disable aliyun.service 2>/dev/null || true

    # Aegis uninstall scripts
    if [ -d "/usr/local/aegis" ]; then
        find /usr/local/aegis -name "uninstall.sh" -exec {} \; 2>/dev/null || true
        find /usr/local/aegis -name "AliYunDunUpdate.sh" -exec {} uninstall \; 2>/dev/null || true
    fi

    # Cloud Monitor uninstall
    if [ -f /usr/local/cloudmonitor/CmsGoAgent.linux-amd64 ]; then
        /usr/local/cloudmonitor/CmsGoAgent.linux-amd64 stop 2>/dev/null || true
        /usr/local/cloudmonitor/CmsGoAgent.linux-amd64 uninstall 2>/dev/null || true
    fi

    rm -rf /usr/local/aegis
    rm -rf /usr/local/cloudmonitor
    rm -f /usr/sbin/aliyun-service
    rm -f /lib/systemd/system/aliyun.service

    # Kill remaining AliYun processes
    pkill -9 -f aliyun-service || true
    pkill -9 -f AliYunDun || true
    pkill -9 -f AliYunDunUpdate || true
fi

systemctl daemon-reload
echo "Alibaba Cloud monitoring tools removed successfully."
