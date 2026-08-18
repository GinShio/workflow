#!/usr/bin/env fish
# -*- mode: fish; sh-shell: fish; coding: utf-8; indent-tabs-mode: nil; -*-
function __ginshio_wsl-setup-environment
    set -g hostip (ip route|awk '/^default/{print $3}')
    set -g loaclip (ip addr show eth0 | grep "inet\b" | awk '{print $2}' | cut -d/ -f1)
    set -x DISPLAY "$hostip:0"
    set -x INPUT_METHOD fcitx
    set -x XMODIFIERS "@im=fcitx"
    set -x GTK_IM_MODULE fcitx
    set -x QT_IM_MODULE fcitx
    daemonize -e /tmp/fcitx5.log -o /tmp/fcitx5.log -p /tmp/fcitx5.pid -l /tmp/fcitx5.pid -a /usr/bin/fcitx5 --disable=wayland
    function proxy
        set -x -g http_proxy  "http://$hostip:8118"
        set -x -g https_proxy "http://$hostip:8118"
    end
    for p in $PATH
        switch (wslpath -m $p)
            case '//wsl*'
                set -p __ginshio_wsl_path $p
            case '*'
                set -p __ginshio_wsl_cleaned_path $p
        end
    end
    set -x -g PATH $__ginshio_wsl_path
end
