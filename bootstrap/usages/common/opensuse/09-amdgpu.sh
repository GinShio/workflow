#!/bin/sh
#@tags: usage:common, scope:system, os:opensuse, gpu:amd

sudo zypper in -y \
    kernel-firmware-amdgpu libdrm_amdgpu1 libdrm_amdgpu1-32bit libvulkan_radeon libvulkan_radeon-32bit \
    radeontop radeontop-lang xf86-video-amdgpu

sudo zypper ar -fcg obs://science:GPU:ROCm openSUSE:ROCm
sudo zypper ref

sudo -E zypper in -y \
    amdsmi hipcc 'libhip*' 'librocalution*' 'librocblas*' 'librocfft*' 'librocm-core*' rocminfo rocm-clang \
    rocm-clang-devel rocm-clang-libs rocm-clang-runtime-devel rocm-cmake rocm-compilersupport-macros rocm-device-libs \
    rocm-hip{,-devel} rocm-libc++-devel rocm-lld rocm-llvm rocm-llvm-devel rocm-llvm-libs rocm-llvm-static rocm-smi \
    roctracer
