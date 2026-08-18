#!/bin/sh
#@tags: domain:dev, type:nightly, dep:git, power:ac
set -eu

cleanup() {
    # shellcheck disable=SC1091
    . "$PROJECTS_SCRIPT_DIR/scripts/unproxy.sh"
}
trap cleanup EXIT

build_projects() {
    _extra_args="$1"
    _projects="$2"

    for proj in $_projects; do
        if ! wits project exists "$proj" >/dev/null 2>&1; then
            continue
        fi

        echo "=> Updating $proj..."
        if wits-update "$proj" --with-borrowed; then
            # Release build (word splitting on _extra_args is intended here)
            wits-build "$proj" --build-type release $_extra_args

            # Debug build
            wits-build "$proj" --build-type debug
        fi
    done
}

if [ "khronos3d" = "${DOTFILES_CURRENT_PROFILE}" ]; then
    build_projects "--install" "amdvlk"
fi

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

build_projects "--install-dir $HOME/.local --install" "mesa spirv-headers spirv-tools slang"
build_projects "" "llvm"
