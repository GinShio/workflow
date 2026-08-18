#!/bin/sh
#@tags: usage:common, scope:system, os:debian
# System: Debian Virtualization & Cross Compilation

# Virtualization & Containerization
# -----------------------------------------------------------------------------
# KVM / QEMU / Libvirt
sudo -E apt install -y \
    qemu-system qemu-user qemu-user-static qemu-utils \
    libvirt-clients libvirt-daemon-system libvirt-dbus \
    bridge-utils virtinst

# Containers (Podman / LXC)
sudo -E apt install -y \
    buildah crun podman podman-docker \
    lxc

# Cross Compilation Toolchains
# Debian provides cross compilers via gcc-<arch>-linux-gnu
# We install a selection similar to the OpenSUSE script
# aarch64 (arm64), arm (armhf), ppc64 (ppc64), ppc64le (ppc64el), riscv64, s390x
sudo -E apt install -y \
    binutils-aarch64-linux-gnu gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
    binutils-arm-linux-gnueabihf gcc-arm-linux-gnueabihf libc6-dev-armhf-cross \
    binutils-powerpc64le-linux-gnu gcc-powerpc64le-linux-gnu libc6-dev-ppc64el-cross \
    binutils-riscv64-linux-gnu gcc-riscv64-linux-gnu libc6-dev-riscv64-cross \
    binutils-s390x-linux-gnu gcc-s390x-linux-gnu libc6-dev-s390x-cross

# Note: ppc64 (Big Endian) might not be standard in all Debian mirrors, skipping to avoid errors 
# unless explicitly needed. ppc64el (Little Endian) is the standard 'ppc64le'.

