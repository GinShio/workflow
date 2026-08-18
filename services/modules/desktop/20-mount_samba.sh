#!/bin/sh
#@tags: domain:desktop, type:autostart, net:samba, dep:sudo, dep:mount
set -eu

trap "sudo -k" EXIT

# POSIX compliant network wait
_network_ready=0
_i=0
while [ "$_i" -lt 30 ]; do
    if ping -c1 -W1 8.8.8.8 >/dev/null 2>&1 || ping -c1 -W1 1.1.1.1 >/dev/null 2>&1; then
        _network_ready=1
        break
    fi
    sleep 1
    _i=$((_i + 1))
done

if [ "$_network_ready" -eq 1 ]; then
    sudo -A -- mount --all --fstab "$HOME/Public/.config.d/$DOTFILES_CURRENT_PROFILE.imm.fstab"
    #sudo -A -- sh -c "nohup mount --all --fstab $HOME/Public/.config.d/$DOTFILES_CURRENT_PROFILE.nohup.fstab &"
else
    echo "Network not available, skipping Samba mounts"
fi
