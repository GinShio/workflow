#!/bin/sh
#@tags: usage:dev, scope:user, dep:fish
# User: Fish Shell

echo "Configuring Fish Shell..."
FISH_TEMP=$(mktemp -d)
(
    cd "$FISH_TEMP" || exit
    curl -o fisher.fish -SL https://github.com/jorgebucaran/fisher/raw/main/functions/fisher.fish
    # Install plugins
    fish -C 'source fisher.fish' -c "fisher install jorgebucaran/fisher IlanCosman/tide PatrickF1/fzf.fish" || true
    # Configure Tide prompt
    fish -c "tide configure --auto --style=Rainbow --prompt_colors='True color' --show_time='24-hour format' --rainbow_prompt_separators=Angled --powerline_prompt_heads=Sharp --powerline_prompt_tails=Sharp --powerline_prompt_style='Two lines, character' --prompt_connection=Disconnected --powerline_right_prompt_frame=No --prompt_spacing=Sparse --icons='Many icons' --transient=No" || true
)
rm -rf "$FISH_TEMP"
