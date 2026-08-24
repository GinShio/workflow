#!/usr/bin/env bash

set -euo pipefail

command -v sassc >/dev/null 2>&1 || {
    echo "build.sh: sassc is required" >&2
    exit 1
}

sassc main.scss main.css

cat \
    '_~magnet_licence.js' \
    '_code.js' \
    '_toc.js' \
    '_$magnet_licence.js' \
    > main.js
