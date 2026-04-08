# feldspar

Cognitive reasoning MCP server for Claude Code — combines sequential thinking, real-time ML learning, and battle-tested patterns into a single installable toolkit.

## Install

```bash
npm install -g feldspar
```

Or use `npx`:

```bash
npx feldspar init
```

## Setup

After installing, run the init wizard in your project:

```bash
feldspar init
```

This copies skill files, hooks, and config to your project's `.claude/` directory and registers the MCP server.

## MCP Server

Feldspar runs as a local MCP server. After `feldspar init`, it is registered automatically. You can also add it manually to `.mcp.json`:

```json
{
  "mcpServers": {
    "feldspar": {
      "command": "feldspar",
      "args": ["serve"]
    }
  }
}
```

## npm Scope

The `@feldspar/` npm scope is used for platform-specific binary packages. These are installed as optional dependencies — only the package matching your platform is downloaded.

## Supported Platforms

| Platform | Package |
|----------|---------|
| Linux x64 | `@feldspar/linux-x64` |
| Linux arm64 | `@feldspar/linux-arm64` |
| macOS x64 | `@feldspar/darwin-x64` |
| macOS arm64 (Apple Silicon) | `@feldspar/darwin-arm64` |
| Windows x64 | `@feldspar/win32-x64` |

## License

MIT
