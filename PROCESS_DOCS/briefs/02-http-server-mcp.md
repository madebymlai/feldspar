# Brief: HTTP Server + MCP Protocol

**Problem**: Claude Code needs to connect to feldspar over HTTP using the MCP protocol. Without a server, there's no way for Claude to send thoughts or receive analysis. This is the transport layer that makes everything else accessible.

**Requirements**:
- HTTP daemon on localhost:3581 using axum + tokio
- Follow the official MCP streamable HTTP transport spec exactly
- JSON-RPC 2.0 with methods: initialize, notifications/initialized, tools/list, tools/call
- One tool registered: `sequentialthinking` (description from `src/tool_description.txt` via `include_str!`)
- Input schema with all ThoughtInput fields (camelCase, matching issue #1 types)
- Full per-thought flow wired with no-ops for modules that don't exist yet (analyzers, warnings, ML, DB)
- Startup: load config → (placeholder: open SQLite → prune → bulk train ML) → start HTTP
- Shutdown: (placeholder: flush traces → save ML → close DB) → exit
- CLI with `clap`: `feldspar start --daemon`
- Concurrent connections from multiple Claude Code sessions
- Never crash

**Constraints**:
- Depends on issue #1 (Types + Config) — done
- Must follow official MCP streamable HTTP transport spec (not a custom HTTP API)
- Analyzer pipeline, warning engine, ML, DB are no-ops — downstream issues (#3-#7) wire real implementations
- `clap` for CLI parsing (`clap = { version = "4", features = ["derive"] }`)
- Error handling: bad JSON → JSON-RPC error, unknown method → error (if has id) or ignore (if notification), tool error → `isError: true`

**Non-goals**:
- No analyzer logic (issue #3)
- No warning engine (issue #4)
- No DB persistence (issue #5)
- No ML inference (issue #6)
- No trace review (issue #7)
- No real thought processing — just parse ThoughtInput, return stub/default ThoughtResult
- No `feldspar init` command yet

**Style**: Production transport layer with placeholder internals. The HTTP/MCP/JSON-RPC layer should be complete and correct. The thought processing behind it is stubbed.

**Key concepts**:
- **MCP streamable HTTP**: The official transport spec for MCP over HTTP (not stdio). Defines how sessions, SSE streams, and JSON-RPC messages flow.
- **JSON-RPC 2.0**: Request/response protocol. Methods have `id` (expect response). Notifications have no `id` (fire and forget).
- **tools/list**: Returns the tool catalog (one tool: sequentialthinking with its schema).
- **tools/call**: Executes the tool. Parses ThoughtInput from params, runs the pipeline, returns ThoughtResult.
- **No-op pipeline**: Placeholder functions matching the signatures that real modules will implement. Returns defaults/empty results. Later issues swap them for real logic without changing the server.
- **Daemon mode**: `feldspar start --daemon` — background process, long-running, survives terminal close.
