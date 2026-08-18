#!/bin/sh
#@tags: usage:common, scope:system, os:debian, gpu:any
# System: Debian GPU Development (X11, Wayland, Vulkan, OpenGL)

# Vulkan Development
sudo apt install -y \
    libvulkan-dev vulkan-tools vulkan-validationlayers spirv-tools glslang-tools

# X11 / Xorg Development
sudo apt install -y \
    libx11-dev{,:i386} libx11-xcb-dev{,:i386} libxcb-dri2-0-dev{,:i386} libxcb-dri3-dev{,:i386} \
    libxcb-glx0-dev{,:i386} libxcb-present-dev{,:i386} libxcb-shm0-dev{,:i386} libxcomposite-dev{,:i386} \
    libxcursor-dev{,:i386} libxdamage-dev{,:i386} libxext-dev{,:i386} libxfixes-dev{,:i386} libxi-dev{,:i386} \
    libxinerama-dev{,:i386} libxkbcommon-dev{,:i386} libxrandr-dev{,:i386} libxrender-dev{,:i386} \
    libxshmfence-dev{,:i386} libxxf86vm-dev{,:i386} x11proto-dev x11proto-gl-dev xorg-dev xserver-xorg-dev \
    xutils-dev

# Wayland Development
sudo apt install -y \
    libglfw3-{dev,wayland} libwayland-dev{,:i386} libwayland-egl-backend-dev wayland-protocols waylandpp-dev

# Mesa / OpenGL / OpenCL
sudo apt install -y \
    freeglut3-dev{,:i386} glslang-{dev,tools} libcairo2-dev{,:i386} libdmx-dev libdrm-dev{,:i386} \
    libegl1-mesa-dev{,:i386} libfontenc-dev{,:i386} libgl1-mesa-dev{,:i386} libglm-dev libglvnd-dev{,:i386} \
    libllvmspirvlib-$(llvm-config --version |awk -F. '{print $1}')-dev libsdl2-dev{,:i386} libslang2-dev{,:i386} \
    libva-dev{,:i386} libvdpau-dev{,:i386} libvulkan-dev libwaffle-dev{,:i386} mesa-common-dev{,:i386} mesa-utils \
    piglit spirv-{cross,tools} vulkan-tools vulkan-validationlayers-dev
