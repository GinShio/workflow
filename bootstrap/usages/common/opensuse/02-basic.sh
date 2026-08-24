#!/bin/sh
#@tags: usage:common, scope:system, os:opensuse
# System: OpenSUSE Base Packages

# Common environment
# -----------------------------------------------------------------------------
# Core Utilities & Shell
sudo zypper in -y \
    bat dash fd figlet fish fzf moreutils neowofetch osdlyrics pandoc-cli patchelf ripgrep tmux

# Archives & Compression
sudo zypper in -y \
    7zip lz4 unzip zip zstd

# Cryptographic & Hashing
sudo zypper in -y \
    b3sum

# Network
sudo zypper in -y \
    aria2 aria2-lang cifs-utils curl privoxy proxychains-ng qbittorrent-nox sshpass wget

# System Utils
sudo zypper in -y \
    cpuinfo cpuinfo-devel

# Font
sudo zypper in -y \
    adobe-sourceserif4-fonts adobe-sourcesans3-fonts adobe-sourcecodepro-fonts \
    adobe-sourcehanserif-cn-fonts adobe-sourcehanserif-hk-fonts \
    adobe-sourcehanserif-jp-fonts adobe-sourcehanserif-kr-fonts \
    adobe-sourcehanserif-tw-fonts adobe-sourcehansans-cn-fonts \
    adobe-sourcehansans-hk-fonts adobe-sourcehansans-jp-fonts \
    adobe-sourcehansans-kr-fonts adobe-sourcehansans-tw-fonts \
    fontawesome-fonts symbols-only-nerd-fonts \
    wqy-bitmap-fonts wqy-microhei-fonts wqy-zenhei-fonts

# Markup
sudo zypper in -y tree-sitter-markdown tree-sitter-rst
