#!/bin/sh
# Stage a built deqp-vk into the gputest runner root. VK-GL-CTS declares no
# install() rule for deqp-vk (only its bundled dependencies have one), so the
# project's install hook does the staging the build system cannot.
set -eu

: "${WITS_BUILD_DIR:?}" "${WITS_INSTALL_DIR:?}" "${WITS_SOURCE_DIR:?}"

# The module's build tree keeps each configuration under its CMake name; the
# data directories are configuration-independent.
case "${WITS_BUILD_TYPE:-debug}" in
    debug) config=Debug ;;
    release) config=Release ;;
    debugoptimized) config=RelWithDebInfo ;;
    minsize|minsizerel) config=MinSizeRel ;;
    *) config=$WITS_BUILD_TYPE ;;
esac

module="$WITS_BUILD_DIR/external/vulkancts/modules/vulkan"
dest=$WITS_INSTALL_DIR

# The installed trees are removed and replaced; a destination inside the
# build tree would therefore delete the source this script reads from.
# Resolve both spellings first: a lexical compare misses `..` and symlink
# aliases.
canonical() {
    realpath -m "$1" 2>/dev/null || printf '%s\n' "$1"
}
build=$(canonical "$WITS_BUILD_DIR")
dest_real=$(canonical "$dest")
case "$dest_real" in
    "$build"|"$build"/*)
        echo "deqp.sh: refusing to install into the build tree ($dest)" >&2
        exit 1
        ;;
esac

# A multi-config generator puts the binary under its configuration name; a
# single-config generator (the project's default is multi-config, but -G may
# override it) writes it beside the module's build files.
deqp_vk="$module/$config/deqp-vk"
[ -x "$deqp_vk" ] || deqp_vk="$module/deqp-vk"

install -d "$dest"
install -m755 "$deqp_vk" "$dest/deqp-vk"

# add_data_dir() already copied the module's data into the build tree; the
# vulkan tree is staged from there. The per-list directory is not installed:
# the suite gets one generated caselist instead.
rm -rf "$dest/vulkan" "$dest/mustpass/vk-default"
cp -a "$module/vulkan" "$dest/vulkan"
install -d "$dest/mustpass"

# The suite's caselist is generated from the build tree's per-list files;
# the individual lists are not installed. The six lists excluded below are
# part of vk-default too (external/vulkancts/mustpass/main/vk-default.txt
# names them): this suite leaves them out deliberately, so the installed
# caselist is vk-default without them. Sort the files so the case order is
# deterministic; awk keeps each file's lines intact.
test -d "$module/vk-default" || {
    echo "deqp.sh: no vk-default case lists in $module" >&2
    exit 1
}
(
    find "$module/vk-default" -type f -name '*.txt' \
        ! -path "$module/vk-default/api.txt" \
        ! -path "$module/vk-default/image/swapchain-mutable.txt" \
        ! -path "$module/vk-default/info.txt" \
        ! -path "$module/vk-default/query-pool.txt" \
        ! -path "$module/vk-default/video.txt" \
        ! -path "$module/vk-default/wsi.txt" \
        | LC_ALL=C sort \
        | while IFS= read -r case_list; do
            awk '1' "$case_list"
        done
) > "$dest/mustpass/vk-default.txt"
test -s "$dest/mustpass/vk-default.txt" || {
    echo "deqp.sh: generated an empty vk-default caselist" >&2
    exit 1
}

install -d "$dest/waivers"
install -m644 "$WITS_SOURCE_DIR/external/vulkancts/mustpass/main/waivers.xml" \
    "$dest/waivers/vk.xml"
