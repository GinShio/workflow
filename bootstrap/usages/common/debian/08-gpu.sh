#!/bin/sh
#@tags: usage:common, scope:system, os:debian, gpu:any
# System: Debian GPU Development (X11, Wayland, Vulkan, OpenGL)

# Vulkan Development
sudo apt install -y \
    libvulkan-dev vulkan-tools vulkan-validationlayers spirv-tools glslang-tools

# X11 / Xorg Development
sudo apt install -y \
    libx11-dev libx11-dev:i386 libx11-xcb-dev libx11-xcb-dev:i386 \
    libxcb-dri2-0-dev libxcb-dri2-0-dev:i386 libxcb-dri3-dev libxcb-dri3-dev:i386 \
    libxcb-glx0-dev libxcb-glx0-dev:i386 libxcb-present-dev libxcb-present-dev:i386 \
    libxcb-shm0-dev libxcb-shm0-dev:i386 libxcomposite-dev libxcomposite-dev:i386 \
    libxcursor-dev libxcursor-dev:i386 libxdamage-dev libxdamage-dev:i386 \
    libxext-dev libxext-dev:i386 libxfixes-dev libxfixes-dev:i386 \
    libxi-dev libxi-dev:i386 libxinerama-dev libxinerama-dev:i386 \
    libxkbcommon-dev libxkbcommon-dev:i386 libxrandr-dev libxrandr-dev:i386 \
    libxrender-dev libxrender-dev:i386 libxshmfence-dev libxshmfence-dev:i386 \
    libxxf86vm-dev libxxf86vm-dev:i386 x11proto-dev x11proto-gl-dev \
    xorg-dev xserver-xorg-dev xutils-dev

# Wayland Development
sudo apt install -y \
    libglfw3-dev libglfw3-wayland libwayland-dev libwayland-dev:i386 \
    libwayland-egl-backend-dev wayland-protocols waylandpp-dev

# Mesa / OpenGL / OpenCL
sudo apt install -y \
    freeglut3-dev freeglut3-dev:i386 glslang-dev glslang-tools \
    libcairo2-dev libcairo2-dev:i386 libdmx-dev libdrm-dev libdrm-dev:i386 \
    libegl1-mesa-dev libegl1-mesa-dev:i386 libfontenc-dev libfontenc-dev:i386 \
    libgl1-mesa-dev libgl1-mesa-dev:i386 libglm-dev libglvnd-dev libglvnd-dev:i386 \
    "libllvmspirvlib-$(llvm-config --version | awk -F. '{print $1}')-dev" \
    libsdl2-dev libsdl2-dev:i386 libslang2-dev libslang2-dev:i386 \
    libva-dev libva-dev:i386 libvdpau-dev libvdpau-dev:i386 \
    libvulkan-dev libwaffle-dev libwaffle-dev:i386 \
    mesa-common-dev mesa-common-dev:i386 mesa-utils \
    piglit spirv-cross spirv-tools vulkan-tools vulkan-validationlayers-dev
