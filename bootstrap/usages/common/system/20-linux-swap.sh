#!/bin/sh
#@tags: usage:common, scope:system, os:linux

if grep -q "/swapfile" /etc/fstab; then
    echo "Swapfile already configured in /etc/fstab."
else
    # Load detection library (assuming it handles FreeBSD correctly)
    . "$PROJECTS_SCRIPT_DIR/scripts/detect.sh"

    # Calculate size (Basic logic: 2x RAM or min 4GB)
    # detect_memory_mb output needs to be safe
    _mem_mb=$(detect_memory_mb 2>/dev/null || echo 2048)
    SETUP_SWAPSIZE=$(echo "$_mem_mb" | awk '{print int($1 / 1024) * 2}')

    # Fallback/Min size 4GB
    if [ "$SETUP_SWAPSIZE" -lt 4 ]; then SETUP_SWAPSIZE=4; fi

    echo "Setting up swapfile ($SETUP_SWAPSIZE GiB) at /swapfile..."

    # Using 4MiB block size
    sudo -A dd if=/dev/zero of=/swapfile bs=4MiB count=$(( SETUP_SWAPSIZE * 256 )) status=progress
    sudo -A chmod 0600 /swapfile
    sudo -A mkswap /swapfile
    sudo -A swapon /swapfile
    echo "/swapfile                                  none       swap  defaults,pri=10  0  0" | sudo -A tee -a /etc/fstab
fi
