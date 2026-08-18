#!/bin/sh

# 1. Priority 1: Environment variable in memory (Bootstrap Phase)
if [ -n "$ROOT_PASSPHRASE" ]; then
    echo "$ROOT_PASSPHRASE"
    exit 0
fi

# 2. Priority 2: Persistent configuration file (Runtime Phase)
# XDG_CONFIG_HOME defaults to $HOME/.config if not set
PROJECT_CONFIG="${XDG_CONFIG_HOME}/workflow/.env"
if [ -f "$PROJECT_CONFIG" ]; then
    # shellcheck disable=SC1090 disable=SC1091
    . "$PROJECT_CONFIG"
    if [ -n "$ROOT_PASSPHRASE" ]; then
        echo "$ROOT_PASSPHRASE"
        exit 0
    fi
fi

# 3. Fallback: Exit with error to force interactive sudo or failure
exit 1
