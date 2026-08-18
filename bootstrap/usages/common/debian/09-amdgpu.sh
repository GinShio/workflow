#!/bin/sh
#@tags: usage:common, scope:system, os:debian, gpu:amd

# Firmware & DRM
# Debian separates non-free firmware. Ensure strict 'non-free-firmware' section is active (handled in repo script).
sudo apt install -y \
    firmware-amd-graphics \
    libdrm-amdgpu1 libdrm-amdgpu1:i386

# Monitoring tools
sudo apt install -y \
    radeontop
