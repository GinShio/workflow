#!/usr/bin/env fish
# -*- mode: fish; sh-shell: fish; coding: utf-8; indent-tabs-mode: nil; -*-

# https://github.com/ErikBjare/dotfiles/blob/master/home/.config/fish/functions/source-posix.fish

# Tries to source a bash env file line by line with fish,
# skips lines that start with '#'
function source-posix
    for i in (cat $argv)
        switch $i
            case '#*'
                #echo "comment $i skipped"
            case ''
                #echo "empty line skipped"
            case '*'
                set -l name (string split -f1 = "$i")
                set -l name_len (string length "$name")
                set name (string replace -r '^export\b[[:space:]]*' '' "$name")
                set -l src_value (string sub -s (math $name_len + 2) "$i")
                set -l value (string split ' ' (eval echo $src_value))
                if set -gq $name; and not set -gq __ginshio_source_var_old_$name
                    set -gx __ginshio_source_var_old_$name $$name
                    set -ge $name
                end
                set -gx "$name" "$value"
        end
    end
end

# Tries to unset a bash env file line by line with fish,
# skips lines that start with '#'
function desource-posix
    set -f unique_array
    for i in (cat $argv)
        switch $i
            case '#*'
                #echo "comment $i skipped"
            case ''
                #echo "empty line skipped"
            case '*'
                set -f name (string split -f1 = "$i" |string replace -r '^export\b[[:space:]]*' '')
                if not contains -- $name $unique_array
                    set -ge $name
                end
                set -a unique_array $name
                if set -gq __ginshio_source_var_old_$name
                    set -l __ginshio_source_var_name __ginshio_source_var_old_$name
                    set -gx $name $$__ginshio_source_var_name
                    set -ge __ginshio_source_var_old_$name
                end
        end
    end
end
