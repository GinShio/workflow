#!/bin/sh
#@tags: usage:common, scope:system, os:opensuse

# Cross compilation
# -----------------------------------------------------------------------------
sudo -E zypper in -y \
    cross-aarch64-binutils cross-aarch64-gcc14 cross-aarch64-linux-glibc-devel \
    cross-arm-binutils cross-arm-gcc14 cross-arm-linux-glibc-devel \
    cross-ppc64-binutils cross-ppc64-gcc14 cross-ppc64-linux-glibc-devel \
    cross-ppc64le-binutils cross-ppc64le-gcc14 cross-ppc64le-linux-glibc-devel \
    cross-riscv64-binutils cross-riscv64-gcc14 cross-riscv64-linux-glibc-devel \
    cross-s390x-binutils cross-s390x-gcc14 cross-s390x-linux-glibc-devel

# Virtualization
# -----------------------------------------------------------------------------
sudo -E zypper in -y -t pattern kvm_tools
sudo -E zypper in -y \
    libvirt libvirt-daemon-lxc libvirt-dbus libvirt-doc qemu qemu-arm qemu-extra qemu-doc qemu-lang qemu-linux-user \
    qemu-ppc qemu-vhost-user-gpu qemu-x86

# Containerization
# -----------------------------------------------------------------------------
sudo -E zypper in -y \
    buildah crun crun-vm libcgroup-tools lxc lxd podman podman-docker \

# riscv64-suse-linux-gcc -march=rv64gc riscv.c
# clang --target=riscv64-suse-linux --sysroot=/usr/riscv64-suse-linux/sys-root -mcpu=generic-rv64 -march=rv64g riscv.c
# QEMU_LD_PREFIX=/usr/riscv64-suse-linux/sys-root c.out
# QEMU_LD_PREFIX=/usr/riscv64-suse-linux/sys-root QEMU_SET_ENV='LD_LIBRARY_PATH=/usr/riscv64-suse-linux/sys-root/lib64:/usr/lib64/gcc/riscv64-suse-linux/14' cc.out
