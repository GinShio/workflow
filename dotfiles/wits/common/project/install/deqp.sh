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
# vulkan tree is staged from there.
rm -rf "$dest/vulkan" "$dest/mustpass"
cp -a "$module/vulkan" "$dest/vulkan"

# vk-default.txt is an index of per-list files, which deqp-runner reads
# recursively (a line ending in .txt names a caselist beside the index). The
# index exists only in the source tree, so the lists are copied from beside
# it and the two always agree.
mustpass="$WITS_SOURCE_DIR/external/vulkancts/mustpass/main"
index="$mustpass/vk-default.txt"
test -s "$index" || {
    echo "deqp.sh: no vk-default index in $mustpass" >&2
    exit 1
}
install -d "$dest/mustpass"

# The suite leaves five of the indexed lists out deliberately: the installed
# index is upstream's without them, and only the lists it names are
# installed. Each must still be named upstream: a rename there would
# otherwise bring its tests back without a word.
excluded=$(printf 'vk-default/%s.txt\n' \
    api image/swapchain-mutable info video wsi)
for list in $excluded; do
    grep -qxF "$list" "$index" || {
        echo "deqp.sh: upstream's vk-default index no longer names $list" >&2
        exit 1
    }
done
grep -vxF "$excluded" "$index" > "$dest/mustpass/vk-default.txt"
while IFS= read -r list; do
    test -s "$mustpass/$list" || {
        echo "deqp.sh: the index names $list, which is missing or empty" >&2
        exit 1
    }
    install -D -m644 "$mustpass/$list" "$dest/mustpass/$list"
done < "$dest/mustpass/vk-default.txt"

install -d "$dest/waivers"
install -m644 "$mustpass/waivers.xml" "$dest/waivers/vk.xml"
