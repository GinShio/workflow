#!/bin/sh
#@tags: usage:vps, scope:system, os:debian, vps:tencent
# System: Remove Tencent Cloud monitoring tools

set -e

echo "Removing Tencent Cloud monitoring tools..."

if [ -d "/usr/local/qcloud" ]; then
    [ -f /usr/local/qcloud/stargate/admin/uninstall.sh ] && /usr/local/qcloud/stargate/admin/uninstall.sh
    [ -f /usr/local/qcloud/YunJing/uninst.sh ] && /usr/local/qcloud/YunJing/uninst.sh
    [ -f /usr/local/qcloud/monitor/barad/admin/uninstall.sh ] && /usr/local/qcloud/monitor/barad/admin/uninstall.sh

    systemctl stop tat_agent 2>/dev/null || true
    systemctl disable tat_agent 2>/dev/null || true
    rm -f /etc/systemd/system/tat_agent.service

    rm -rf /usr/local/qcloud

    # Kill remaining agent processes
    ps -A | grep -E 'tat_agent|barad_agent|sgagent|ydservice' | awk '{print $1}' | xargs -I@ kill -9 @ 2>/dev/null || true
fi

systemctl daemon-reload
echo "Tencent Cloud monitoring tools removed successfully."
