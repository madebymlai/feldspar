---
name: fs-config
description: Configure feldspar principles and thinking modes. Use after feldspar init.
---

# Feldspar Configuration

You are helping the user configure feldspar principles and thinking modes.
Use the `configure` MCP tool for all operations.

## Flow

1. Start by calling `configure` with `action: "list"` and `level: "project"` to show current state.
2. Ask the user: "Would you like to configure at the **user level** (applies to all your projects) or **project level** (just this project)?"
3. Show current principles groups with their active/inactive status.
4. Ask what they want to change:
   - Activate or deactivate principle groups
   - Add custom principle groups with rules
   - Add custom thinking modes (auto-creates matching agent)
5. For each change, call `configure` with the appropriate action.
6. After all changes, call `configure` with `action: "list"` again to confirm.
7. Note: changes take effect on next Claude Code session restart.

## Examples

Activate a principle group:
```
configure({action: "activate", level: "project", group: "security"})
```

Add a custom principle:
```
configure({action: "add_group", level: "project", group: "our-standards", active: true})
configure({action: "add_principle", level: "project", group: "our-standards", name: "No raw SQL", rule: "Use ORM for all queries", ask: ["Am I writing raw SQL?"]})
```

Add a custom thinking mode (auto-creates agent):
```
configure({action: "add_mode", level: "project", name: "data-pipeline", budget: "standard", requires: [], watches: "pipeline jobs"})
```

## Rules

- Always call `list` first so you know the current state
- Ask the user before making changes — don't assume
- One change at a time — confirm each before moving on
- Remind the user to restart Claude Code session after changes
