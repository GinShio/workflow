#!/bin/sh
#@tags: usage:common, scope:system, os:debian
# System: Debian Base Packages

# Common environment
# -----------------------------------------------------------------------------
# Core Utilities & Shell
sudo -E apt install -y \
    bat dash fd-find figlet fish flatpak fzf git git-doc git-lfs moreutils neofetch pandoc patchelf ripgrep tmux zstd

# Archives & Compression
sudo -E apt install -y \
    7zip lz4 unzip zip

# Network
sudo -E apt install -y \
    cifs-utils curl privoxy proxychains4 sshpass wget

# System Utils
sudo -E apt install -y \
    cpuinfo

# Font
sudo -E apt install -y \
    fonts-noto-cjk fonts-noto-color-emoji fonts-font-awesome fonts-wqy-zenhei fonts-wqy-microhei
