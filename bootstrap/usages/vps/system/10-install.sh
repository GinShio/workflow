#!/bin/sh
#@tags: usage:vps, scope:system, os:debian
# System: Install base packages for VPS

set -eu

echo "Installing base packages (socat, wget, curl, certbot, nginx, fail2ban, git)..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y socat wget curl certbot nginx git fail2ban podman podman-compose uidmap dbus-user-session

if { [ -n "${DNS_PROVIDER:-}" ] && [ -z "${DNS_API_TOKEN:-}" ]; } ||
   { [ -z "${DNS_PROVIDER:-}" ] && [ -n "${DNS_API_TOKEN:-}" ]; }; then
    echo "Error: DNS_PROVIDER and DNS_API_TOKEN must be configured together." >&2
    exit 1
fi

if [ -n "${DNS_PROVIDER:-}" ]; then
    case "$DNS_PROVIDER" in
        *[!a-z0-9-]*)
            echo "Error: DNS_PROVIDER must use lowercase letters, digits, or '-'." >&2
            exit 1
            ;;
    esac
    echo "DNS provider ($DNS_PROVIDER) configured. Installing corresponding Certbot DNS plugin..."
    # Handle specific plugin names for CN providers or standard ones
    case "$DNS_PROVIDER" in
        aliyun)
            apt-get install -y python3-pip
            python3 -m pip install --break-system-packages certbot-dns-aliyun
            ;;
        dnspod|tencent)
            apt-get install -y python3-pip
            python3 -m pip install --break-system-packages \
                git+https://github.com/tengattack/certbot-dns-dnspod.git
            ;;
        *)
            apt-get install -y "python3-certbot-dns-$DNS_PROVIDER"
            ;;
    esac
fi

echo "Base packages installed successfully."
