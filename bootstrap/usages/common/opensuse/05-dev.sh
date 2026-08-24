#!/bin/sh
#@tags: usage:dev, scope:system, os:opensuse

# Common develop utils
# -----------------------------------------------------------------------------
sudo zypper in -y -t pattern devel_basis
sudo zypper in -y \
    bison chrpath dwarves flex graphviz git git-delta git-doc git-lfs hugo libxslt-tools re2c ShellCheck sqlite3 \
    tree-sitter xmlto

# C++ environment
# -----------------------------------------------------------------------------
# Core Toolchain
sudo zypper in -y -t pattern devel_C_C++
# Unwrapped braces
sudo zypper in -y \
    ccache conan doxygen imake include-what-you-use sccache tree-sitter-c tree-sitter-cpp

# Compiler toolchain (GCC & Clang)
sudo zypper in -y \
    clang clang-doc clang-extract clang-tools clang-devel \
    gcc gcc-32bit gcc-c++ gcc-c++-32bit gcc-info gcovr gdb \
    lcov lld lldb llvm llvm-doc llvm-opt-viewer llvm-devel tree-sitter-tablegen \
    mold wild

# Build-system
sudo zypper in -y \
    cmake kf6-extra-cmake-modules \
    meson tree-sitter-meson \
    ninja

# Libraries - Core
sudo zypper in -y \
    cli11-devel eigen3-devel eigen3-doc 'libboost_*-devel' \
    libc++-devel libc++abi-devel libcaca-devel \
    libelf-devel libelf-devel-32bit \
    libexpat-devel libexpat-devel-32bit \
    libopenssl-devel libopenssl-devel-32bit \
    libpciaccess-devel \
    libstdc++-devel libstdc++-devel-32bit \
    libunwind-devel \
    libxml2-devel libxml2-devel-32bit \
    libzstd-devel libzstd-devel-32bit

# Libraries - Other
sudo zypper in -y \
    nanomsg-devel ncurses-devel ncurses-devel-32bit poco-devel \
    readline-devel readline-devel-32bit spdlog-devel stb-devel tinyobjloader-devel \
    z3-devel zlib-ng-compat-devel

# Python3 environment
# -----------------------------------------------------------------------------
# Interpreter & Core
sudo zypper in -y \
    python3 python3-devel python3-doc python3-pipx python3-ruff python3-virtualenv tree-sitter-python

# AI / ML / Math
sudo zypper in -y \
    libonnx onnx-devel python3-numpy python3-numpy-devel python3-onnx python3-sympy

# Build & Packaging
sudo zypper in -y \
    python3-distutils-extra python3-packaging python3-setuptools python3-u-msgpack-python

# Utilities & Libraries
sudo zypper in -y \
    python3-Jinja2 python3-Mako python3-PyYAML python3-Sphinx python3-astunparse python3-cryptography \
    python3-cryptography-vectors python3-filelock python3-fsspec python3-lit python3-lxml python3-lz4 python3-psutil \
    python3-pybind11 python3-pybind11-devel python3-pyelftools python3-pygit2 python3-pylint python3-pytest \
    python3-requests python3-ruamel.yaml python3-typing_extensions

# Programmings environment (Other)
# -----------------------------------------------------------------------------
# Rust
sudo zypper in -y \
    cargo rust rust-bindgen tree-sitter-rust

# Zig
sudo zypper in -y \
    tree-sitter-zig zig zig-libs zls

# Java
sudo zypper in -y \
    java-21-openjdk java-21-openjdk-devel \
    java-25-openjdk java-25-openjdk-devel

# Node.js
sudo zypper in -y \
    nodejs-common pnpm typescript deno

# Functional (Erlang/Elixir/Haskell)
sudo zypper in -y \
    elixir elixir-doc elixir-hex erlang erlang-doc \
    ghc ghc-doc ghc-manual ghc-prof tree-sitter-haskell

# Others
sudo zypper in -y \
    lua lua-devel

# TeX
sudo zypper in -y 'texlive-*'

# Emacs
# -----------------------------------------------------------------------------
sudo zypper in -y emacs emacs-nox emacs-x11 libtool libvterm-tools libvterm-devel
