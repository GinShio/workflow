#!/usr/bin/env fish
# -*- mode: fish; sh-shell: fish; coding: utf-8; indent-tabs-mode: nil; -*-

source {{@@ _dotfile_abs_dst @@}}/functions/__ginshio_source-posix.fish

if status is-interactive
    # Commands to run in interactive sessions can go here
    set --universal fish_greeting
    source {{@@ _dotfile_abs_dst @@}}/create_abbrs.fish
    source {{@@ _dotfile_abs_dst @@}}/functions/__ginshio_login_envvars.fish
    source {{@@ _dotfile_abs_dst @@}}/functions/__ginshio_zswap-statistics.fish
end
