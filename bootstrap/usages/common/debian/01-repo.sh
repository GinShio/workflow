#!/bin/sh
#@tags: usage:common, scope:system, os:debian
# System: Debian Repos & Update

# update source
# -----------------------------------------------------------------------------
sudo -E apt install -y apt-transport-https ca-certificates
. /etc/os-release

cat <<-EOF | sudo tee /etc/apt/sources.list
deb https://mirrors.shanghaitech.edu.cn/debian/ ${VERSION_CODENAME} main contrib non-free non-free-firmware
# deb-src https://mirrors.shanghaitech.edu.cn/debian/ ${VERSION_CODENAME} main contrib non-free non-free-firmware

deb https://mirrors.shanghaitech.edu.cn/debian/ ${VERSION_CODENAME}-updates main contrib non-free non-free-firmware
# deb-src https://mirrors.shanghaitech.edu.cn/debian/ ${VERSION_CODENAME}-updates main contrib non-free non-free-firmware

deb https://mirrors.shanghaitech.edu.cn/debian/ ${VERSION_CODENAME}-backports main contrib non-free non-free-firmware
# deb-src https://mirrors.shanghaitech.edu.cn/debian/ ${VERSION_CODENAME}-backports main contrib non-free non-free-firmware

deb https://mirrors.shanghaitech.edu.cn/debian-security ${VERSION_CODENAME}-security main contrib non-free non-free-firmware
# deb-src https://mirrors.shanghaitech.edu.cn/debian-security ${VERSION_CODENAME}-security main contrib non-free non-free-firmware
EOF

sudo dpkg --add-architecture i386 && sudo -E apt update && sudo -E apt dist-upgrade -y

# Cleanups
sudo apt purge akonadi-server mariadb-common mariadb-server mariadb-client redis || true
sudo apt-mark hold akonadiconsole
