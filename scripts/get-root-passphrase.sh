#!/bin/sh

# 1. Priority 1: Environment variable in memory (Bootstrap Phase)
if [ -n "${ROOT_PASSPHRASE:-}" ]; then
    printf '%s\n' "$ROOT_PASSPHRASE"
    exit 0
fi

# 2. Priority 2: private raw credential deployed by dotfiles/wits/manifest.toml.
PASSPHRASE_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/wits/root-passphrase"
if [ -r "$PASSPHRASE_FILE" ]; then
    IFS= read -r _passphrase < "$PASSPHRASE_FILE" || true
    if [ -n "${_passphrase:-}" ]; then
        printf '%s\n' "$_passphrase"
        exit 0
    fi
fi

# 3. Fallback: Exit with error to force interactive sudo or failure
exit 1
