#!/usr/bin/env fish
# -*- mode: fish; sh-shell: fish; coding: utf-8; indent-tabs-mode: nil; -*-
if command -v systemctl >/dev/null 2>&1
    for env_var in (systemctl --user show-environment)
        set -l kv (string split -m 1 = -- $env_var)
        if test (count $kv) -eq 2
            set -l key $kv[1]
            set -l val $kv[2]

            if test "$key" = "$PATH"
                fish_add_path (string split : -- $val)
            else if not set -q $key
                set -gx $key $val
            end
        end
    end
end
