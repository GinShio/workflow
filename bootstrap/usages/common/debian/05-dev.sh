#!/bin/sh
#@tags: usage:dev, scope:system, os:debian
# System: Debian Dev Packages

# Common develop utils
# -----------------------------------------------------------------------------
sudo -E apt install -y build-essential
sudo -E apt install -y \
    bison chrpath dwarves flex graphviz git git-doc git-lfs hugo libxslt1-dev re2c shellcheck sqlite3 \
    xmlto

# C++ environment
# -----------------------------------------------------------------------------
# Core Toolchain
sudo -E apt install -y binutils build-essential ccache cmake conan doxygen gcovr gdb \
    iwyu lcov meson mold ninja-build sccache

# Compilers (GCC & Clang)
sudo -E apt install -y \
    clang clangd clang-format clang-tidy clang-tools \
    gcc g++ g++-multilib libclang-dev lld lldb llvm llvm-dev \
    gcc-multilib

# Libraries - Core
sudo -E apt install -y \
    libboost1.81-all-dev libc++-dev libc++abi-dev libcaca-dev libcaca-dev:i386 \
    libcli11-dev libelf-dev libelf-dev:i386 libexpat1-dev libexpat1-dev:i386 \
    libssl-dev libssl-dev:i386 libunwind-14-dev libunwind-14-dev:i386 \
    libxml2-dev libxml2-dev:i386 libzip-dev libzip-dev:i386 libzstd-dev libzstd-dev:i386

# Libraries - Other
sudo -E apt install -y \
    libnanomsg-dev libncurses5-dev libpciaccess-dev libpciaccess-dev:i386 \
    libpoco-dev libreadline-dev libreadline-dev:i386 libspdlog-dev libspdlog-dev:i386 \
    libstb-dev libtinyobjloader-dev libudev-dev libudev-dev:i386 \
    libz3-dev zlib1g-dev

# Python3 environment
# -----------------------------------------------------------------------------
# Interpreter & Core
sudo -E apt install -y \
    python3 python3-dev python3-doc python3-pip python3-pip-whl python3-pipx python3-ruff python3-virtualenv

# AI / ML / Math
sudo -E apt install -y \
    libonnx-dev python3-numpy python3-numpy-dev python3-onnx python3-sympy

# Build & Packaging
sudo -E apt install -y \
    python3-distutils-extra python3-packaging python3-setuptools python3-u-msgpack

# Utilities & Libraries
sudo -E apt install -y \
    python3-jinja2 python3-mako python3-yaml python3-sphinx python3-astunparse python3-cryptography python3-cryptography-vectors \
    python3-filelock python3-fsspec python3-lxml python3-lz4 python3-psutil python3-pybind11 \
    python3-pyelftools python3-pygit2 python3-pylint python3-pytest python3-requests python3-ruamel.yaml \
    python3-typing-extensions

# Programmings environment (Other)
# -----------------------------------------------------------------------------
# Rust
sudo -E apt install -y \
    cargo rust-all librust-bindgen-dev

# Zig (Debian unstable/testing usually, or stable-backports, might be missing in old stable)
# Zig is often just 'zig'
sudo -E apt install -y zig

# Java
sudo -E apt install -y \
    openjdk-21-jdk openjdk-21-jre openjdk-21-source

# Node.js
sudo -E apt install -y \
    nodejs npm yarnpkg

# Functional (Erlang/Elixir/Haskell)
sudo -E apt install -y \
    elixir erlang ghc ghc-doc ghc-prof

# Others
sudo -E apt install -y \
    lua5.4 liblua5.4-dev

# TeX
sudo -E apt install -y texlive-full

# Emacs
# -----------------------------------------------------------------------------
sudo -E apt install -y emacs-gtk emacs-nox libtool libvterm-dev
