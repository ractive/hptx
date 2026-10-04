#!/bin/bash
# Dump the current TUI rendering (the LCD as text) to stdout.
# Trailing blanks and empty LCD rows are stripped; pass -a for the raw pane.
if [ "${1:-}" = -a ]; then exec tmux capture-pane -p -t calc; fi
tmux capture-pane -p -t calc | sed 's/ *$//' | grep -v -E '^│ *│$'
