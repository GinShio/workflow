#!/bin/sh
#@tags: usage:common, scope:system, os:freebsd

# Check if swap is configured in fstab
if grep -q "swap" /etc/fstab | grep -q "file="; then
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

    echo "Setting up swapfile ($SETUP_SWAPSIZE GiB) at /usr/swap0..."

    # FreeBSD preferred location often /usr/swap0 if / is small, but let's use /usr/swap0 to be safe/standard
    SWAPFILE="/usr/swap0"

    # Create file: 1m block size is standard on BSD
    sudo -A dd if=/dev/zero of="$SWAPFILE" bs=1m count=$(( SETUP_SWAPSIZE * 1024 )) status=progress

    # Set permissions (0600)
    sudo -A chmod 0600 "$SWAPFILE"

    # Add to /etc/fstab
    # Format: device mountpoint type options dump pass
    # We use md99 to avoid conflict with auto-created md devices
    echo "md99	none	swap	sw,file=$SWAPFILE,late	0	0" | sudo -A tee -a /etc/fstab

    # Activate immediately
    # We don't rely on fstab for immediate activation in script to avoid parsing issues
    sudo -A swapon -aL
fi

