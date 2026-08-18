#!/bin/sh
#@tags: usage:common, scope:system, os:opensuse, de:kde

# Source
sudo zypper ar -fcg obs://KDE:Extra openSUSE:kDE:Extra
sudo -E zypper ref
sudo -E zypper dup -y --allow-vendor-change

# kDE environment
# -----------------------------------------------------------------------------
sudo -E zypper in -y libplasma6-devel
sudo -E zypper in -y -t pattern devel_qt6 devel_kde_frameworks6

sudo zypper in -y \
    fcitx5 fcitx5-rime filelight filelight-lang freerdp-wayland kdeconnect-kde kdeconnect-kde-lang krdc krdc-lang \
    krfb krfb-lang kvantum-manager kvantum-manager-lang pam_kwallet6 partitionmanager partitionmanager-lang
