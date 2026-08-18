#!/bin/sh
#@tags: usage:dev, scope:system
# System: User Groups

echo "Adding user to groups..."
sudo -A usermod -aG kvm,libvirt,render,video "$(whoami)" || echo "Warning: Failed to add user to some groups."
