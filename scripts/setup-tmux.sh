#!/bin/bash
# Feldspar tmux setup: configures tmux + Claude Code for teammate pane splitting.
# Run once. Linux/macOS only. Windows users: use in-process mode (Shift+Down).
#
# What it does:
#   1. Checks tmux is installed
#   2. Adds mouse support to ~/.tmux.conf
#   3. Adds teammateMode: "tmux" to ~/.claude.json
#   4. Starts a tmux session named "feldspar" and launches Claude Code inside it
#
# Usage: ./scripts/setup-tmux.sh
# After setup: teammates auto-split into tmux panes. Click to switch.
