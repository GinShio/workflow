#!/bin/sh
#@tags: usage:common, scope:system, os:freebsd

SWAPFILE="/usr/swap0"
FSTAB_ENTRY="md99	none	swap	sw,file=$SWAPFILE,late	0	0"

if grep -Fq "file=$SWAPFILE" /etc/fstab; then
    echo "Swapfile already configured in /etc/fstab."
else
    _swap_created=0
    _fstab_added=0
    rollback_swap() {
        _status=$?
        trap - EXIT HUP INT TERM
        if [ "$_status" -ne 0 ]; then
            if [ "$_fstab_added" -eq 1 ]; then
                _fstab_tmp=$(mktemp) || _fstab_tmp=
                if [ -n "$_fstab_tmp" ]; then
                    grep -Fvx "$FSTAB_ENTRY" /etc/fstab > "$_fstab_tmp" || true
                    sudo -A install -m 0644 "$_fstab_tmp" /etc/fstab || true
                    rm -f "$_fstab_tmp"
                fi
            fi
            _swap_active=0
            swapinfo -k 2>/dev/null |
                awk '$1 == "/dev/md99" { found=1 } END { exit !found }' &&
                _swap_active=1
            if [ "$_swap_active" -eq 1 ] &&
               ! sudo -A swapoff /dev/md99; then
                echo "Warning: /dev/md99 is still active; keeping $SWAPFILE." >&2
                _swap_created=0
            fi
            if [ "$_swap_created" -eq 1 ]; then
                sudo -A rm -f "$SWAPFILE" || true
            fi
        fi
        exit "$_status"
    }
    trap rollback_swap EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    . "$PROJECTS_SCRIPT_DIR/scripts/detect.sh"

    _mem_mb=$(detect_memory_mb 2>/dev/null || echo 2048)
    SETUP_SWAPSIZE=$(echo "$_mem_mb" | awk '{print int($1 / 1024) * 2}')

    if [ "$SETUP_SWAPSIZE" -lt 4 ]; then SETUP_SWAPSIZE=4; fi

    echo "Setting up swapfile ($SETUP_SWAPSIZE GiB) at /usr/swap0..."

    if [ -e "$SWAPFILE" ]; then
        echo "Error: $SWAPFILE exists but is not declared in /etc/fstab; refusing to overwrite it." >&2
        exit 1
    fi

    _swap_created=1
    sudo -A dd if=/dev/zero of="$SWAPFILE" bs=1m count=$(( SETUP_SWAPSIZE * 1024 )) status=progress
    sudo -A chmod 0600 "$SWAPFILE"

    _fstab_added=1
    printf '%s\n' "$FSTAB_ENTRY" | sudo -A tee -a /etc/fstab >/dev/null
    sudo -A swapon -aL
fi

