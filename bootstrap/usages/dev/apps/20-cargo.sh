#!/bin/sh
#@tags: usage:dev, scope:apps, dep:cargo
# Apps: Cargo

echo "Installing Cargo packages..."
packages="
    git-branchless
"

for pkg in $packages; do
    cargo install "$pkg" --locked || echo "Warning: Failed to install $pkg in cargo"
done
