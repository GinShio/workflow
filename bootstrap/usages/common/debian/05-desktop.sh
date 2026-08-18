#!/bin/sh
#@tags: usage:common, scope:system, os:debian, de:any
# Desktop / GUI-only packages (shared across DEs)

# Web
sudo -E apt install -y \
    firefox-esr thunderbird qbittorrent

# Multimedia
sudo -E apt install -y \
    inkscape imagemagick-6-common mpv obs-studio osdlyrics
