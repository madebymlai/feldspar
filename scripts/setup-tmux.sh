#!/usr/bin/env bash
set -e

# Skip on Windows
if [[ "$(uname -s)" == MINGW* ]] || [[ "$(uname -s)" == CYGWIN* ]]; then
    echo "Skipping tmux setup on Windows"
    exit 0
fi

# Check tmux installed
if ! command -v tmux &>/dev/null; then
    echo "tmux not installed — skipping team mode setup"
    echo "Install tmux for agent team support: sudo apt install tmux (Linux) or brew install tmux (macOS)"
    exit 0
fi

# Add mouse support
TMUX_CONF="$HOME/.tmux.conf"
if ! grep -q "set -g mouse on" "$TMUX_CONF" 2>/dev/null; then
    echo "set -g mouse on" >> "$TMUX_CONF"
    echo "Added mouse support to $TMUX_CONF"
fi

# Add teammateMode to claude config
CLAUDE_CONF="$HOME/.claude.json"
if [ -f "$CLAUDE_CONF" ]; then
    if ! grep -q "teammateMode" "$CLAUDE_CONF"; then
        if command -v python3 &>/dev/null; then
            python3 -c "
import json
with open('$CLAUDE_CONF', 'r') as f: d = json.load(f)
d['teammateMode'] = 'tmux'
with open('$CLAUDE_CONF', 'w') as f: json.dump(d, f, indent=2)
"
            echo "Set teammateMode: tmux in $CLAUDE_CONF"
        fi
    fi
else
    echo '{"teammateMode": "tmux"}' > "$CLAUDE_CONF"
    echo "Created $CLAUDE_CONF with teammateMode: tmux"
fi
