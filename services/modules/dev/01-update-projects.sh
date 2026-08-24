#!/bin/sh
#@tags: domain:dev, type:nightly, dep:git, dep:wits, power:ac
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
        if wits update "$proj" --with-borrowed; then
            _main_branch=$(wits project main-branch "$proj")
            # Release build (word splitting on _extra_args is intended here)
            wits build "$proj" --build-type release $_extra_args "-b$_main_branch"

            # Debug build
            wits build "$proj" --build-type debug "-b$_main_branch"
        fi
    done
}

case ":${DOTFILES_OVERLAYS:-}:" in
    *":khronos3d:"*)
        build_projects "--install" "amdvlk"
        ;;
esac

# shellcheck disable=SC1091
. "$PROJECTS_SCRIPT_DIR/scripts/proxy.sh"

build_projects "--install-dir $HOME/.local --install" "mesa spirv-headers spirv-tools slang"
build_projects "" "llvm"
