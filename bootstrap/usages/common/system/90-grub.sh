#!/bin/sh
#@tags: usage:common, scope:system, dep:grub

GRUB_MARKER="# DOTFILES_BRINGUP_GRUB_CONFIG"
if [ -f /etc/default/grub ]; then
    if ! grep -qF "$GRUB_MARKER" /etc/default/grub; then
        echo "Patching GRUB config for drop-in support..."
        cat <<-EOF | sudo -A tee -a /etc/default/grub
$GRUB_MARKER
# Safe include of /etc/default/grub.d/*.conf
if [ -d /etc/default/grub.d ]; then
  for _f in /etc/default/grub.d/*.conf; do
    [ -r "\$_f" ] || continue
    _tmp=\$(mktemp /tmp/grub-dropin.XXXXXX) || continue
    awk '/^[A-Z_][A-Z0-9_]*[[:space:]]*=/{print}' "\$_f" > "\$_tmp"
    if [ -s "\$_tmp" ]; then
      # shellcheck disable=SC1090 disable=SC1091
      . "\$_tmp"
    fi
    rm -f "\$_tmp"
  done
  unset _f _tmp
fi
EOF
        # Ideally we should update grub here, but command differs by distro (update-grub vs grub2-mkconfig)
        # Leaving it for user or distro script handle if specific execution needed.
    else
        echo "GRUB config already patched."
    fi
else
    echo "Warning: /etc/default/grub not found. Skipping GRUB patching."
fi
