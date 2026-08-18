#!/bin/sh
#@tags: domain:desktop, type:autostart, hw:laptop, dep:powerprofilesctl
set -eu

# Only configure power profiles on laptops
powerprofilesctl configure-action --enable amdgpu_dpm || true
powerprofilesctl configure-action --enable amdgpu_panel_power || true
