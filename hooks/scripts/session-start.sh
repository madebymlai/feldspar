#!/usr/bin/env bash
# SessionStart hook for feldspar
# 1. Auto-starts feldspar daemon if not running
# 2. Injects orchestrator context into Claude's session

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
PROJECT_ROOT="$(cd "${HOOKS_DIR}/.." && pwd)"

# Auto-start daemon if not running
if ! curl -s http://localhost:3581/health > /dev/null 2>&1; then
  "${PROJECT_ROOT}/target/release/feldspar" start --daemon &
  sleep 1
fi

# Skip orchestrator injection for teammates -- they get their role from agent prompts
if [ -n "${CLAUDE_CODE_TEAM_NAME:-}" ]; then
  exit 0
fi

# Read orchestrator context (main session only)
context=$(cat "${SCRIPT_DIR}/orchestrator-context.md" 2>&1 || echo "Error reading orchestrator context")

# Escape for JSON
escape_for_json() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\r'/\\r}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s"
}

escaped=$(escape_for_json "$context")

# Output context injection (Claude Code format)
printf '{\n  "hookSpecificOutput": {\n    "hookEventName": "SessionStart",\n    "additionalContext": "%s"\n  }\n}\n' "$escaped"

exit 0
