#!/bin/sh
#@tags: usage:common, scope:system, dep:loginctl

loginctl enable-linger "$(whoami)"
