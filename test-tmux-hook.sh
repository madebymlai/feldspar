#!/bin/bash
# Simulates what a SessionStart hook would do
# Run this OUTSIDE tmux to test

if [ -z "$TMUX" ]; then
    echo "Not in tmux. Relaunching inside tmux..."
    tmux kill-session -t feldspar-test 2>/dev/null
    exec tmux new-session -s feldspar-test "echo 'Claude would run here'; bash"
else
    echo "Already in tmux. Proceeding normally."
fi
