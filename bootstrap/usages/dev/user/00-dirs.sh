#!/bin/sh
#@tags: usage:dev, scope:user
# User: Directories

echo "Creating directory structure..."
mkdir -p "$HOME/Projects"
mkdir -p "$HOME/.local/bin" "$HOME/.local/share" "$HOME/.local/lib64"
mkdir -p "$HOME/.local/share/fonts" "$HOME/.local/share/applications"
