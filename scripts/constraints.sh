#!/bin/sh
#
# Execution constraints for the recurring service modules.
# Callers provide the detected CURRENT_* values and a callback for the
# scheduling namespaces (power/schedule).

constraint_validate_tag() {
    _cvt_tag=$1

    case "$_cvt_tag" in
        domain:?*|type:?*)
            return 0
            ;;
        state:enabled|state:disabled)
            return 0
            ;;
        os:?*|gpu:?*|cpu:?*|de:?*|dep:?*)
            return 0
            ;;
        hw:laptop|power:ac|power:battery|power:any)
            return 0
            ;;
        schedule:daily|schedule:weekly|schedule:monthly)
            return 0
            ;;
        schedule:[1-9]*[dhm])
            _cvt_value=${_cvt_tag#schedule:}
            _cvt_number=${_cvt_value%?}
            case "$_cvt_number" in
                ''|*[!0-9]*) ;;
                *) return 0 ;;
            esac
            ;;
    esac

    printf 'Error: unknown or malformed module tag: %s\n' "$_cvt_tag" >&2
    return 1
}

constraints_validate_file() {
    _cvf_file=$1

    if ! tags_validate "$_cvf_file"; then
        printf 'Error: %s must have a shebang on line 1 and #@tags on line 2\n' \
            "$_cvf_file" >&2
        return 1
    fi

    for _cvf_tag in $(tags_get "$_cvf_file"); do
        constraint_validate_tag "$_cvf_tag" || {
            printf 'Error: invalid tag in %s\n' "$_cvf_file" >&2
            return 1
        }
    done
}

constraints_validate_tree() {
    _cvt_dir=$1
    [ -d "$_cvt_dir" ] || {
        printf 'Error: module directory does not exist: %s\n' "$_cvt_dir" >&2
        return 1
    }

    if ! _cvt_files=$(find "$_cvt_dir" -type f -name '*.sh' -print); then
        printf 'Error: could not enumerate modules below %s\n' "$_cvt_dir" >&2
        return 1
    fi
    _cvt_old_ifs=$IFS
    IFS='
'
    set -f
    for _cvt_file in $_cvt_files; do
        constraints_validate_file "$_cvt_file" || {
            IFS=$_cvt_old_ifs
            set +f
            return 1
        }
    done
    IFS=$_cvt_old_ifs
    set +f
}

constraint_has_word() {
    _chw_words=$1
    _chw_want=$2
    case " $_chw_words " in
        *" $_chw_want "*) return 0 ;;
        *) return 1 ;;
    esac
}

constraint_match_system_tag() {
    _cmst_tag=$1

    case "$_cmst_tag" in
        os:*)
            _cmst_want=${_cmst_tag#os:}
            [ "$_cmst_want" = "$CURRENT_OS" ] ||
                { [ "$CURRENT_OS" = linux ] &&
                  { [ "$_cmst_want" = "$CURRENT_DISTRO" ] ||
                    { [ "$_cmst_want" = debian ] &&
                      [ "$CURRENT_DISTRO" = ubuntu ]; }; }; }
            ;;
        gpu:any)
            [ -n "$GPU_VENDORS" ]
            ;;
        gpu:*)
            constraint_has_word "$GPU_VENDORS" "${_cmst_tag#gpu:}"
            ;;
        cpu:*)
            [ "${_cmst_tag#cpu:}" = "$CPU_VENDOR" ]
            ;;
        de:any)
            [ "$CURRENT_DE" != headless ]
            ;;
        de:*)
            [ "${_cmst_tag#de:}" = "$CURRENT_DE" ]
            ;;
        hw:laptop)
            [ "$IS_LAPTOP" -eq 1 ]
            ;;
        dep:*)
            command -v "${_cmst_tag#dep:}" >/dev/null 2>&1
            ;;
        *)
            return 2
            ;;
    esac
}

# constraints_match_file <file> <runner-specific-callback>
#
# Multiple os:* tags are alternatives. Every other constraint is conjunctive.
# The callback returns 0 for a match, 1 for a mismatch, and 2 when a namespace
# does not belong to that runner.
constraints_match_file() {
    _cmf_file=$1
    _cmf_callback=$2
    _cmf_tags=$(tags_get "$_cmf_file")
    _cmf_os_seen=0
    _cmf_os_matched=0

    for _cmf_tag in $_cmf_tags; do
        case "$_cmf_tag" in
            state:disabled)
                return 1
                ;;
            state:enabled|domain:*|type:*)
                ;;
            os:*)
                _cmf_os_seen=1
                if constraint_match_system_tag "$_cmf_tag"; then
                    _cmf_os_matched=1
                fi
                ;;
            gpu:*|cpu:*|de:*|hw:*|dep:*)
                constraint_match_system_tag "$_cmf_tag" || return 1
                ;;
            power:*|schedule:*)
                "$_cmf_callback" "$_cmf_tag" "$_cmf_file"
                _cmf_status=$?
                case "$_cmf_status" in
                    0) ;;
                    1) return 1 ;;
                    *)
                        printf 'Error: tag %s is not valid for %s\n' \
                            "$_cmf_tag" "$_cmf_file" >&2
                        return 2
                        ;;
                esac
                ;;
            *)
                printf 'Error: unvalidated tag %s in %s\n' \
                    "$_cmf_tag" "$_cmf_file" >&2
                return 2
                ;;
        esac
    done

    if [ "$_cmf_os_seen" -eq 1 ] && [ "$_cmf_os_matched" -eq 0 ]; then
        return 1
    fi
    return 0
}
