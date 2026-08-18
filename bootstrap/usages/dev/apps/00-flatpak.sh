#!/bin/sh
#@tags: usage:dev, scope:apps, dep:flatpak
# Apps: Flatpak

echo "Installing Flatpaks..."
packages="
    com.discordapp.Discord
    com.visualstudio.code
"

# Ensure flathub repo exists (idempotent usually)
flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo || true
for pkg in $packages; do
    flatpak install -y flathub "$pkg" || echo "Warning: Failed to install $pkg in flatpak"
done
