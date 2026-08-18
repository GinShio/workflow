#!/bin/sh
#
# Library for managing script metadata via magic comments.
# Format Requirement:
#   Line 1: Shebang (e.g., #!/bin/sh)
#   Line 2: #@tags: tag1, tag2, tag3
#

# tags_get <file_path>
# Output: list of tags (space separated)
tags_get() {
    [ -f "$1" ] || return 0

    # Read strict line 2, look for #@tags: prefix
    # Use awk to normalize comma/space delimiters
    sed -n '2p' "$1" 2>/dev/null | awk '
        /^#@tags:/ {
            sub(/^#@tags:[[:space:]]*/, "");
            gsub(/,[[:space:]]*/, " ");
            gsub(/,/, " ");
            print
        }
    '
}

# tags_has <file_path> <tag>
# Returns 0 if file has tag, 1 otherwise
tags_has() {
    _th_tags=$(tags_get "$1")
    for _th_t in $_th_tags; do
        if [ "$_th_t" = "$2" ]; then
            return 0
        fi
    done
    return 1
}

# tags_find_any <directory> <tag1> [tag2...]
# Find files matching ANY of the given tags
tags_find_any() {
    _tfa_dir="$1"
    shift

    [ -d "$_tfa_dir" ] || return 1

    # Use find to list all files, then filter logic in loop
    # Note: Logic inside pipe runs in subshell
    find "$_tfa_dir" -type f | while read -r _tfa_file; do
        _tfa_file_tags=$(tags_get "$_tfa_file")
        [ -z "$_tfa_file_tags" ] && continue

        _tfa_found=0
        for _tfa_arg in "$@"; do
            for _tfa_ft in $_tfa_file_tags; do
                if [ "$_tfa_ft" = "$_tfa_arg" ]; then
                    _tfa_found=1
                    break 2
                fi
            done
        done

        if [ "$_tfa_found" -eq 1 ]; then
            echo "$_tfa_file"
        fi
    done
}

# tags_find_all <directory> <tag1> [tag2...]
# Find files matching ALL of the given tags
tags_find_all() {
    _tfa_dir="$1"
    shift

    [ -d "$_tfa_dir" ] || return 1

    find "$_tfa_dir" -type f | while read -r _tfa_file; do
        _tfa_file_tags=$(tags_get "$_tfa_file")
        [ -z "$_tfa_file_tags" ] && continue

        _tfa_missing=0
        for _tfa_arg in "$@"; do
            _tfa_arg_found=0
            for _tfa_ft in $_tfa_file_tags; do
                if [ "$_tfa_ft" = "$_tfa_arg" ]; then
                    _tfa_arg_found=1
                    break
                fi
            done
            if [ "$_tfa_arg_found" -eq 0 ]; then
                _tfa_missing=1
                break
            fi
        done

        if [ "$_tfa_missing" -eq 0 ]; then
            echo "$_tfa_file"
        fi
    done
}

# tags_list_all <directory>
# List all unique tags used in a directory
tags_list_all() {
    _tla_dir="$1"
    [ -d "$_tla_dir" ] || return 1

    find "$_tla_dir" -type f | while read -r _tla_file; do
        tags_get "$_tla_file"
    done | tr ' ' '\n' | grep -v '^$' | sort -u
}

# tags_validate <file_path>
# Check if file adheres to the tag format (Shebang + Tags)
# Returns 0 if valid, 1 otherwise
tags_validate() {
    [ -f "$1" ] || return 1

    # Check Shebang on line 1
    _tv_l1=$(sed -n '1p' "$1")
    case "$_tv_l1" in
        \#\!*) ;;
        *) return 1 ;;
    esac

    # Check Tags on line 2
    _tv_l2=$(sed -n '2p' "$1")
    case "$_tv_l2" in
        \#@tags:*) return 0 ;;
        *) return 1 ;;
    esac
}
