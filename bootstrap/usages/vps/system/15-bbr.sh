#!/bin/sh
#@tags: usage:vps, scope:system, os:debian
# System: Configure BBR for VPS

set -e

echo "Configuring BBR..."

# Check if BBR is already enabled
if sysctl net.ipv4.tcp_congestion_control | grep -q bbr; then
    echo "BBR is already enabled."
    exit 0
fi

# Load tcp_bbr module
modprobe tcp_bbr
echo "tcp_bbr" > /etc/modules-load.d/bbr.conf

# Configure sysctl
cat <<-EOF > /etc/sysctl.d/99-bbr.conf
net.core.default_qdisc=cake
net.ipv4.tcp_congestion_control=bbr
EOF

# Apply sysctl settings
sysctl --system

# Verify BBR is enabled
if sysctl net.ipv4.tcp_congestion_control | grep -q bbr; then
    echo "BBR has been successfully enabled."
else
    echo "Failed to enable BBR. Please check your kernel version (requires 4.9+)."
fi
