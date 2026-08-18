#!/bin/sh
#@tags: usage:common, scope:system, os:opensuse
# System: OpenSUSE Repos & Update

# update source
# -----------------------------------------------------------------------------
### zypper
sudo zypper rr --all
# NJU Mirrors
sudo zypper ar -fcg https://mirrors.nju.edu.cn/opensuse/tumbleweed/repo/oss NJU:oss
sudo zypper ar -fcg https://mirrors.nju.edu.cn/opensuse/tumbleweed/repo/non-oss NJU:non-oss
sudo zypper ar -fcg https://mirror.nju.edu.cn/packman/suse/openSUSE_Tumbleweed NJU:packman
# Extras
sudo zypper ar -fcg obs://Virtualization openSUSE:Virtualization
sudo zypper ar -fcg https://download.opensuse.org/repositories/devel:/tools:/compiler/openSUSE_Factory openSUSE:compiler
# sudo zypper ar -fcg https://download.opensuse.org/repositories/server:/messaging/openSUSE_Factory openSUSE:messaging
# sudo zypper ar -fcg https://download.opensuse.org/repositories/utilities/openSUSE_Factory openSUSE:Utilities

sudo -E zypper ref
sudo -E zypper dup -y --allow-vendor-change

# Cleanups
sudo zypper remove -u -y valkey mariadb mariadb-client akonadi gitk || true
sudo zypper al cmake-gui git-gui akonadi gitk
