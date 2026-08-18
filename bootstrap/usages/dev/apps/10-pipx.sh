#!/bin/sh
#@tags: usage:dev, scope:apps, dep:python3, dep:pipx
# Apps: Pipx

echo "Installing Pipx packages..."
# Ensure pipx path is fine
pipx ensurepath || true
packages="
    dotdrop
    iree-base-compiler[onnx]
    pyright
    trash-cli
"

for pkg in $packages; do
    # Extract package name for check (e.g. iree-base-compiler[onnx] -> iree-base-compiler)
    # But pipx list is slow. Let's just try install, pipx skips if installed.
    pipx install "$pkg" || echo "Warning: Failed to install $pkg in pipx"
done
