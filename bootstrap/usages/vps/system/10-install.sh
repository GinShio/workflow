#!/bin/sh
#@tags: usage:vps, scope:system, os:debian
# System: Install base packages for VPS

set -e

echo "Installing base packages (socat, wget, curl, certbot, nginx, fail2ban, git)..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y socat wget curl certbot nginx git fail2ban podman podman-compose uidmap dbus-user-session

if [ -n "${DNS_PROVIDER:-}" ]; then
    echo "DNS provider ($DNS_PROVIDER) configured. Installing corresponding Certbot DNS plugin..."
    # Handle specific plugin names for CN providers or standard ones
    case "$DNS_PROVIDER" in
        aliyun)
            # Aliyun usually requires a third-party plugin or pip installation
            apt-get install -y python3-pip
            pip3 install --break-system-packages certbot-dns-aliyun || echo "Warning: Failed to install certbot-dns-aliyun via pip."
            ;;
        dnspod|tencent)
            # DNSPod/Tencent usually requires a third-party plugin or pip installation
            sudo pip install --break-system-packages git+https://github.com/tengattack/certbot-dns-dnspod.git \
                || echo "Warning: Failed to install certbot-dns-dnspod via pip."
            ;;
        *)
            # Standard certbot plugins available in Debian/Ubuntu repos
            apt-get install -y "python3-certbot-dns-$DNS_PROVIDER" || {
                echo "Warning: Failed to install python3-certbot-dns-$DNS_PROVIDER via apt."
            }
            ;;
    esac
fi

echo "Base packages installed successfully."
