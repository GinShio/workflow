#!/bin/sh
#@tags: domain:desktop, type:autostart, dep:sudo, dep:mount
set -eu

trap "sudo -k" EXIT

# dotfiles/samba deploys one `<overlay>.imm.fstab` per overlay. A host stacks
# several overlays, so mount every fstab it actually received instead of
# picking a single one out of the list.
[ -n "${DOTFILES_OVERLAYS:-}" ] || {
    echo "No Dotfiles overlays are active; skipping Samba mounts."
    exit 0
}

_mounted=0
_old_ifs=$IFS
IFS=:
set -f
for _overlay in $DOTFILES_OVERLAYS; do
    _fstab="$HOME/Public/.config.d/$_overlay.imm.fstab"
    [ -f "$_fstab" ] || continue
    # `mount` reports the actual SMB/network failure. A public ICMP probe is
    # not a reliable proxy for whether a private file server is reachable.
    sudo -A -- mount --all --fstab "$_fstab"
    _mounted=1
done
IFS=$_old_ifs
set +f

if [ "$_mounted" -eq 0 ]; then
    echo "No Samba fstab for overlays '$DOTFILES_OVERLAYS'; nothing mounted."
fi
