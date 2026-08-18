#!/bin/sh
#@tags: usage:vps, scope:services, os:debian
# Services: Enable and start VPS login services

set -e

# Nginx
if systemctl list-unit-files | grep -q nginx.service; then
    echo "Enabling Nginx..."
    systemctl enable --now nginx.service
fi

# Fail2ban
if systemctl list-unit-files | grep -q fail2ban.service; then
    echo "Enabling Fail2ban..."
    systemctl enable --now fail2ban.service
fi

# SSH
if systemctl is-active --quiet sshd.service || systemctl is-enabled --quiet sshd.service 2>/dev/null; then
    echo "Restarting SSHd..."
    systemctl restart sshd.service
elif systemctl is-active --quiet ssh.service || systemctl is-enabled --quiet ssh.service 2>/dev/null; then
    echo "Restarting SSH..."
    systemctl restart ssh.service
fi
