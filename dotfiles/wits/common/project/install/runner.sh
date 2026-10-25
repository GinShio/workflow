#!/bin/sh
# Stage the runner binaries cargo built. The cargo backend has no install step,
# so the project's install hook replaces it and copies the binaries instead.
set -eu

: "${WITS_BUILD_DIR:?}" "${WITS_INSTALL_DIR:?}"

# cargo writes each profile under its own name; follow the configured build
# type.
case "${WITS_BUILD_TYPE:-debug}" in
    debug) profile=debug ;;
    release) profile=release ;;
    *) profile=$WITS_BUILD_TYPE ;;
esac

dest=$WITS_INSTALL_DIR
install -d "$dest"
for bin in deqp-runner piglit-runner; do
    install -m755 "$WITS_BUILD_DIR/$profile/$bin" "$dest/$bin"
done
