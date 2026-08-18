#!/bin/sh
#@tags: usage:common, scope:system, os:opensuse, de:any
# Desktop / GUI-only packages (shared across DEs)

# Web browsers & mail
sudo zypper in -y \
    MozillaFirefox MozillaThunderbird qbittorrent

# Multimedia / graphics
sudo zypper in -y \
    inkscape ImageMagick mpv obs-studio
