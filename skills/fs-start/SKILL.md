---
name: fs-start
description: Initialize feldspar session. Call temper MCP with orchestrator role to load agent instructions.
---

# Feldspar Session Start

Initialize this session by loading orchestrator instructions.

## Steps

1. Load the `mcp__feldspar__temper` tool via ToolSearch if not already loaded.
2. Call `temper` with role `orchestrator`.
3. Follow the returned instructions for the rest of this session.
