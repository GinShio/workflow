#!/bin/sh
#@tags: usage:dev, scope:services, dep:systemctl
# Services: System Services

sudo -A systemctl daemon-reload

SERVICES="
    libvirtd
    virtlockd
    virtlogd
    lxc-net
    sshd
    systemd-tmpfiles-clean
"

echo "Enabling System Services..."
for svc in $SERVICES; do
    # Check if unit file exists to avoid error spam
    if systemctl list-unit-files "$svc*" | grep -q "$svc"; then
        sudo -A systemctl enable --now "$svc" || echo "Warning: Failed to enable $svc"
    else
        echo "Info: Service $svc not found, skipping."
    fi
done

# Virsh network
if command -v virsh >/dev/null 2>&1; then
    sudo -A virsh net-autostart default || true
fi
