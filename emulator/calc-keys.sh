#!/bin/bash
# Send keystrokes to the emulated calculator via the TUI, one key at a time.
# Usage: calc-keys <keys...>   e.g.  calc-keys ';' ';' s e r v e r Enter
# Each argument is one tmux key name: letters a-z, digits, '.', '+', '-', '*',
# '/', Enter, BSpace, Up/Down/Left/Right, F1..F7.
# TUI map: '[' left-shift, ']' right-shift, ';' alpha, '\' ON (also Escape),
#          letters = the calculator key labelled with that (alpha) letter.
# KEY_DELAY (seconds, default 0.2) is the pause between keys.
set -eu
for k in "$@"; do
  # A bare ';' is tmux's command separator and must be escaped.
  [ "$k" = ";" ] && k='\;'
  tmux send-keys -t calc -- "$k"
  sleep "${KEY_DELAY:-0.2}"
done
