#!/bin/sh
#
# The one file format bootstrap reads: `key: value`, one per line, `#` for a
# comment.
#
# Everything declarative here uses it — unit metadata and registry entries —
# so there is one parser to trust and one syntax to learn. It is deliberately
# not TOML: bootstrap runs on a machine that has nothing but a POSIX shell,
# before the toolchain that could build a real parser exists, so the format
# has to be readable with one `awk` pass.

# meta_field <file> <key>
#
# Prints the value, or nothing when the file or the key is absent. Blanks
# around the value are stripped; blanks inside it are kept, because some
# values are shell fragments.
#
# The first colon separates key from value, so a value may contain colons.
meta_field() {
    [ -f "$1" ] || return 0

    awk -v key="$2" '
        /^[ \t]*#/ { next }
        {
            i = index($0, ":")
            if (i == 0) next
            k = substr($0, 1, i - 1)
            gsub(/^[ \t]+|[ \t]+$/, "", k)
            if (k != key) next
            v = substr($0, i + 1)
            sub(/^[ \t]+/, "", v)
            sub(/[ \t]+$/, "", v)
            print v
            exit
        }
    ' "$1"
}

# meta_list <file> <key>
#
# A comma-separated field, one item per line. Prints nothing when the field is
# absent or empty.
meta_list() {
    meta_field "$1" "$2" | tr ',' '\n' | sed 's/^[ \t]*//; s/[ \t]*$//' |
        grep -v '^$' || true
}

# meta_keys <file>
#
# Every key the file sets, one per line. Used to reject typos rather than let
# a misspelled key read as an absent one.
meta_keys() {
    [ -f "$1" ] || return 0

    awk '
        /^[ \t]*#/ { next }
        {
            i = index($0, ":")
            if (i == 0) next
            k = substr($0, 1, i - 1)
            gsub(/^[ \t]+|[ \t]+$/, "", k)
            if (k != "") print k
        }
    ' "$1"
}
