#!/bin/sh
#@tags: usage:common, scope:system, os:linux

if grep -q "/swapfile" /etc/fstab; then
    echo "Swapfile already configured in /etc/fstab."
else
    _swap_created=0
    _fstab_added=0
    _fstab_entry="/swapfile                                  none       swap  defaults,pri=10  0  0"
    rollback_swap() {
        _status=$?
        trap - EXIT HUP INT TERM
        if [ "$_status" -ne 0 ]; then
            if [ "$_fstab_added" -eq 1 ]; then
                _fstab_tmp=$(mktemp) || _fstab_tmp=
                if [ -n "$_fstab_tmp" ]; then
                    grep -Fvx "$_fstab_entry" /etc/fstab > "$_fstab_tmp" || true
                    sudo -A install -m 0644 "$_fstab_tmp" /etc/fstab || true
                    rm -f "$_fstab_tmp"
                fi
            fi
            _swap_active=0
            awk '$1 == "/swapfile" { found=1 } END { exit !found }' /proc/swaps &&
                _swap_active=1
            if [ "$_swap_active" -eq 1 ] &&
               ! sudo -A swapoff /swapfile; then
                echo "Warning: /swapfile is still active; keeping the file." >&2
                _swap_created=0
            fi
            if [ "$_swap_created" -eq 1 ]; then
                sudo -A rm -f /swapfile || true
            fi
        fi
        exit "$_status"
    }
    trap rollback_swap EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    . "$PROJECTS_SCRIPT_DIR/scripts/detect.sh"

    # Calculate size (Basic logic: 2x RAM or min 4GB)
    # detect_memory_mb output needs to be safe
    _mem_mb=$(detect_memory_mb 2>/dev/null || echo 2048)
    SETUP_SWAPSIZE=$(echo "$_mem_mb" | awk '{print int($1 / 1024) * 2}')

    # Fallback/Min size 4GB
    if [ "$SETUP_SWAPSIZE" -lt 4 ]; then SETUP_SWAPSIZE=4; fi

    echo "Setting up swapfile ($SETUP_SWAPSIZE GiB) at /swapfile..."

    if [ -e /swapfile ]; then
        echo "Error: /swapfile exists but is not declared in /etc/fstab; refusing to overwrite it." >&2
        exit 1
    fi

    _swap_created=1
    sudo -A dd if=/dev/zero of=/swapfile bs=4MiB count=$(( SETUP_SWAPSIZE * 256 )) status=progress
    sudo -A chmod 0600 /swapfile
    sudo -A mkswap /swapfile
    sudo -A swapon /swapfile
    _fstab_added=1
    printf '%s\n' "$_fstab_entry" | sudo -A tee -a /etc/fstab >/dev/null
fi
