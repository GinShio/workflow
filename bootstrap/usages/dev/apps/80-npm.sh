#!/bin/sh
#@tags: usage:dev, scope:apps, dep:deno
# Apps: Node.js

echo "Installing Node.js packages..."
packages="
    @anthropic-ai/claude-code
    @openai/codex
"

for pkg in $packages; do
    DENO_INSTALL_ROOT="$HOME/.local/bin" deno install --global -A --allow-scripts npm:$pkg || echo "Warning: Failed to install $pkg in deno"
done
