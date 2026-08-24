#!/bin/sh
#@tags: usage:dev, scope:apps, dep:meson, dep:cargo, dep:ninja
# Apps: Wits

set -eu

WITS_SOURCE_DIR="${WITS_SOURCE_DIR:-$PROJECTS_SCRIPT_DIR/wits}"

if [ ! -f "$WITS_SOURCE_DIR/meson.build" ]; then
    echo "Error: Missing Wits source directory at ${WITS_SOURCE_DIR}"
    exit 1
fi

echo "Building and installing Wits..."
(
    cd "$WITS_SOURCE_DIR"

    if [ -f _build/meson-private/coredata.dat ]; then
        meson setup _build . --reconfigure -Dbuildtype=release --prefix "$HOME/.local"
    else
        meson setup _build . -Dbuildtype=release --prefix "$HOME/.local"
    fi

    meson compile -C _build
    meson install -C _build
)

"$HOME/.local/bin/wits" __applets >/dev/null
