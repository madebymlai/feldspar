# Solution Design: HTTP Server + MCP Protocol

## 1. Executive Summary

Implement the HTTP daemon and MCP protocol layer for feldspar. This is the transport that connects Claude Code to the thought processor. axum server on localhost:3581, MCP streamable HTTP transport (spec 2025-11-25), JSON-RPC 2.0 dispatch, one tool (`sequentialthinking`), full per-thought flow wired with no-ops for downstream modules. CLI via clap with `feldspar start --daemon`.

## 2. Rationale

| Decision | Rationale | Alternative | Why Rejected |
|----------|-----------|-------------|--------------|
| `application/json` responses, no SSE on POST | Pipeline completes in microseconds — no streaming needed. Spec allows either `application/json` or `text/event-stream`; we pick the simpler one. | SSE for all responses | Complexity for zero benefit |
| GET /mcp returns 405 | No server-initiated messages needed. Spec says server MUST return SSE stream OR 405. We return 405. | Full SSE listener endpoint | No use case — feldspar never pushes unsolicited messages |
| DELETE /mcp returns 405 | Session cleanup handled server-side on timeout/shutdown. Spec says server MAY return 405. | Client-driven session termination | Unnecessary complexity for MVP |
| Session ID via UUID | Spec requires `Mcp-Session-Id` on stateful servers. UUID is simple, crypto-secure with `uuid::v4`. | JWT sessions | Over-engineered for localhost-only server |
| New `src/mcp.rs` module | Separates MCP protocol handling from server lifecycle. SRP — main.rs handles CLI/startup, mcp.rs handles protocol. | Everything in main.rs | main.rs would become 500+ lines |
| `clap` derive for CLI | Clean, idiomatic, extensible for future commands (`init`, `status`). | `std::env::args()` | Hand-rolling arg parsing for zero benefit |
| No-op pipeline in `thought.rs` | Matches the signatures real modules will implement. Later issues swap no-ops for real logic without changing the server. | Stub responses without pipeline | Wouldn't validate the actual data flow |
| No daemonization crate | Use simple `std::process::Command` self-relaunch + detach for `--daemon`. No `fork()` crate needed. | `daemonize` crate | Extra dependency for one fork operation |
| Origin validation middleware | MCP spec MUST. Reject requests with foreign Origin headers (DNS rebinding prevention). Accept no-Origin (non-browser) and localhost origins. | No validation | Spec violation, security risk |
| `WireResponse` struct for tool output | Wire format merges echo-backs + trace metadata + ThoughtResult. Separate type prevents shipping wrong-shaped responses. | Serialize ThoughtResult directly | Missing traceId, thoughtNumber, wrong field names |
| `trace_id` field on ThoughtInput | Required for multi-thought trace correlation. Client receives traceId in response, sends it back on subsequent thoughts. | Session-based implicit trace lookup | Prevents multiple concurrent traces per session |
| Protocol version `2025-11-25` | Current MCP spec version. Implement version negotiation per lifecycle spec. | Hardcode without negotiation | Spec requires negotiation |
| JSON-RPC errors return HTTP 200 | JSON-RPC 2.0 over HTTP: protocol errors go in the body, not HTTP status. HTTP 4xx only for transport-level issues. | HTTP 400 for all errors | Violates JSON-RPC over HTTP convention |

## 3. Technology Stack

| Component | Crate | Version | Purpose |
|-----------|-------|---------|---------|
| HTTP server | `axum` | 0.8 | Already in Cargo.toml |
| Async runtime | `tokio` | 1 (full) | Already in Cargo.toml |
| Serialization | `serde`, `serde_json` | 1 | Already in Cargo.toml |
| UUID | `uuid` | 1 (v4) | Already in Cargo.toml. Session IDs. |
| CLI | `clap` | 4 (derive) | **New dependency** |
| Config | (from issue #1) | — | `Config::load()`, `Arc<Config>` |
| Types | (from issue #1) | — | `ThoughtInput`, `ThoughtResult`, `ThinkingServer` |

**Cargo.toml addition:**
```toml
clap = { version = "4", features = ["derive"] }
```

## 4. Architecture

### Data Flow

```
CLI: feldspar start [--daemon] [--port 3581]
  → if --daemon: relaunch self without --daemon, detach, exit parent
  → Config::load("config/feldspar.toml", "config/principles.toml")
  → ThinkingServer::new(config)
  → build axum router
  → bind to 127.0.0.1:{port}
  → serve with graceful shutdown (SIGINT/SIGTERM)

HTTP request flow:
  ALL requests → Origin validation middleware (see Security below)
  GET  /health → 200 {"status": "ok"}
  POST /mcp    → JSON-RPC dispatch (see below)
  GET  /mcp    → 405 Method Not Allowed
  DELETE /mcp  → 405 Method Not Allowed

Origin validation middleware:
  → if Origin header absent → allow (non-browser clients, curl, Claude Code)
  → if Origin matches http://localhost:* or http://127.0.0.1:* → allow
  → else → 403 Forbidden (DNS rebinding protection, MCP spec MUST)

JSON-RPC dispatch (POST /mcp):
  → parse body as JSON
  → if parse fails → HTTP 200 + JSON-RPC error -32700 (no id)
  → if array (batch):
      → if empty array → HTTP 200 + JSON-RPC error -32600
      → process each message, collect responses
      → if all notifications (no responses) → HTTP 202
      → else → HTTP 200 + array of responses (requests only, not notifications)
  → if single message:
      → if notification (no id): handle + return 202
      → if response (has result/error): return 202
      → if request (has id + method): dispatch:
          "initialize"    → parse params.protocolVersion, negotiate, return capabilities + session ID
          "notifications/initialized" → 202 (notification)
          "tools/list"    → tool catalog
          "tools/call"    → validate params.name, deserialize params.arguments as ThoughtInput
          "ping"          → pong
          unknown         → HTTP 200 + JSON-RPC error -32601
  → all JSON-RPC responses (including errors) return HTTP 200
  → HTTP 4xx only for transport-level issues (400 missing session, 403 bad origin, 404 expired session, 405 wrong method)
```

### Per-Thought Flow (tools/call)

```
tools/call arrives:
  → validate params.name == "sequentialthinking" (else JSON-RPC -32602)
  → deserialize params.arguments as ThoughtInput (else JSON-RPC -32602)
  → validate thinking_mode against config.modes (if Some)
  → if thought_number == 1 AND trace_id is None: create new Trace, generate UUID
  → if trace_id is Some: lookup Trace in ThinkingServer.traces (else JSON-RPC -32602 "unknown trace")
  → append ThoughtRecord to trace
  → no-op: run_analyzer_pipeline() → (empty Vec<Alert>, default Observations)
  → no-op: ml_predict() → (None, None)
  → no-op: generate_warnings() → empty Vec<String>
  → no-op: generate_recap() → None
  → build WireResponse by merging:
      echo-backs from ThoughtInput: thought_number, total_thoughts, next_thought_needed
      trace metadata: trace_id, branches (from trace), thought_history_length
      ThoughtResult fields (renamed): warnings, alerts, confidence_calculated,
        depth_overlap, budget_used, budget_max, budget_category,
        ml_trajectory → trajectory, ml_drift → drift_detected,
        recap, adr, auto_outcome
  → no-op: tokio::spawn(db_write_thought()) → does nothing
  → if next_thought_needed == false:
      → no-op: generate_adr() → None
      → no-op: tokio::spawn(trace_review()) → does nothing
      → no-op: tokio::spawn(ml_train()) → does nothing
      → mark trace closed
  → serialize WireResponse as JSON string
  → return MCP tool result: { content: [{ type: "text", text: "<json>" }], isError: false }
```

### Module Catalog

**`src/main.rs`** — CLI + server lifecycle

| Component | Role |
|-----------|------|
| `Cli` struct (clap derive) | Parse `start --daemon --port` |
| `main()` | CLI dispatch, daemon relaunch, config load, server start |
| `run_server()` | Build router, bind, serve with graceful shutdown |
| Signal handler | SIGINT/SIGTERM → graceful shutdown |

**`src/mcp.rs`** (new) — MCP protocol layer

| Component | Role |
|-----------|------|
| `McpState` | Shared state: `ThinkingServer` + session map |
| `handle_post()` | Parse JSON-RPC, dispatch to method handlers |
| `handle_get()` | Return 405 |
| `handle_delete()` | Return 405 |
| `validate_origin()` | Middleware: check Origin header, reject foreign origins with 403 |
| `handle_initialize()` | Parse params.protocolVersion, negotiate version, return capabilities, generate session ID |
| `handle_tools_list()` | Return sequentialthinking tool definition |
| `handle_tools_call()` | Validate params.name, deserialize params.arguments as ThoughtInput, run no-op pipeline, build WireResponse |
| `handle_ping()` | Return empty result |
| `validate_session()` | Check Mcp-Session-Id header |

**`src/thought.rs`** — Add methods to existing types

| Addition | Role |
|----------|------|
| `ThoughtInput.trace_id` | Add `trace_id: Option<String>` field with `#[serde(default)]` — required for multi-thought trace correlation |
| `WireResponse` | New struct (Serialize, camelCase): merges echo-backs + trace metadata + renamed ThoughtResult fields |
| `ThinkingServer::process_thought()` | No-op pipeline: create/lookup trace, build WireResponse |
| `Trace::new()` | Constructor with UUID and timestamp |

## 5. Protocol/Schema

### MCP Server Capabilities (initialize response)

The server parses `params.protocolVersion` from the client's initialize request. Supported versions: `["2025-11-25"]`. If the client requests an unsupported version, respond with JSON-RPC error `-32602` with `data: { supported: ["2025-11-25"], requested: "<client_version>" }`.

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2025-11-25",
    "capabilities": {
      "tools": {}
    },
    "serverInfo": {
      "name": "feldspar",
      "version": "0.1.0"
    }
  }
}
```

Header: `Mcp-Session-Id: <uuid>`

### Tool Definition (tools/list response)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tools": [
      {
        "name": "sequentialthinking",
        "description": "<contents of src/tool_description.txt>",
        "inputSchema": {
          "type": "object",
          "properties": {
            "traceId": { "type": "string", "description": "Trace ID returned from thought 1. Required for thought 2+." },
            "thought": { "type": "string", "description": "Your current reasoning step." },
            "thoughtNumber": { "type": "integer", "description": "Current step (1-indexed)." },
            "totalThoughts": { "type": "integer", "description": "Estimated total." },
            "nextThoughtNeeded": { "type": "boolean", "description": "True if more thinking needed." },
            "thinkingMode": { "type": "string", "description": "Domain mode." },
            "affectedComponents": { "type": "array", "items": { "type": "string" } },
            "confidence": { "type": "number", "description": "0-100 confidence." },
            "evidence": { "type": "array", "items": { "type": "string" } },
            "estimatedImpact": {
              "type": "object",
              "properties": {
                "latency": { "type": "string" },
                "throughput": { "type": "string" },
                "risk": { "type": "string" }
              }
            },
            "isRevision": { "type": "boolean" },
            "revisesThought": { "type": "integer" },
            "branchFromThought": { "type": "integer" },
            "branchId": { "type": "string" },
            "needsMoreThoughts": { "type": "boolean" }
          },
          "required": ["thought", "thoughtNumber", "totalThoughts", "nextThoughtNeeded"]
        }
      }
    ]
  }
}
```

### Tool Result (tools/call response)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"traceId\":\"550e8400...\",\"thoughtNumber\":1,\"totalThoughts\":5,\"nextThoughtNeeded\":true,\"warnings\":[],\"alerts\":[],\"budgetUsed\":1,\"budgetMax\":5,\"budgetCategory\":\"standard\"}"
      }
    ],
    "isError": false
  }
}
```

### JSON-RPC Error Codes

| Code | Meaning | When |
|------|---------|------|
| -32700 | Parse error | Invalid JSON |
| -32600 | Invalid request | Missing jsonrpc/method fields |
| -32601 | Method not found | Unknown method |
| -32602 | Invalid params | Bad ThoughtInput, unknown tool name |
| -32603 | Internal error | Server error (should never happen) |

### WireResponse (the actual tool output)

```rust
/// Flat wire response — merges echo-backs, trace metadata, and ThoughtResult.
/// This is what Claude sees in content[0].text. NOT ThoughtResult directly.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireResponse {
    // Echo-backs from ThoughtInput
    pub trace_id: String,
    pub thought_number: u32,
    pub total_thoughts: u32,
    pub next_thought_needed: bool,

    // Trace metadata
    pub branches: Vec<String>,
    pub thought_history_length: usize,

    // From ThoughtResult (renamed where needed)
    pub warnings: Vec<String>,
    pub alerts: Vec<Alert>,
    pub confidence_reported: Option<f64>,       // from input.confidence
    pub confidence_calculated: Option<f64>,
    pub confidence_gap: Option<f64>,            // derived: |reported - calculated|
    pub bias_detected: Option<String>,
    pub sycophancy: Option<String>,
    pub depth_overlap: Option<f64>,
    pub budget_used: u32,
    pub budget_max: u32,
    pub budget_category: String,
    pub trajectory: Option<f64>,                // from ThoughtResult.ml_trajectory
    pub drift_detected: Option<bool>,           // from ThoughtResult.ml_drift
    pub recap: Option<String>,

    // Completion-only fields
    pub adr: Option<String>,
    pub trust_score: Option<f64>,
    pub trust_reason: Option<String>,
}
```

### HTTP Status Codes

| Status | When |
|--------|------|
| 200 | All JSON-RPC responses (including error responses) |
| 202 | Accepted notification/response (no body) |
| 400 | Transport-level: missing session ID |
| 403 | Transport-level: invalid Origin header |
| 404 | Transport-level: expired/unknown session ID |
| 405 | GET or DELETE on /mcp |

### Required Headers

**Client → Server:**
- `Content-Type: application/json`
- `Accept: application/json, text/event-stream`
- `Mcp-Session-Id: <uuid>` (after initialization)

**Server → Client:**
- `Content-Type: application/json`
- `Mcp-Session-Id: <uuid>` (on initialize response only)

## 6. Implementation Details

### File Structure

```
src/main.rs     ← CLI (clap), server startup/shutdown, axum router, signal handling
src/mcp.rs      ← NEW: MCP protocol handlers, JSON-RPC dispatch, session management
src/thought.rs  ← ADD: ThinkingServer::process_thought(), Trace::new()
Cargo.toml      ← ADD: clap = { version = "4", features = ["derive"] }
```

### Integration Points

- `src/main.rs` imports `mcp::create_router()` to build the axum router
- `mcp.rs` imports `ThinkingServer`, `ThoughtInput`, `ThoughtResult`, `Config` from issue #1 types
- `mcp.rs` holds `McpState` as axum `State`: `Arc<McpState>` containing `ThinkingServer` + sessions
- `ThinkingServer::process_thought()` is the no-op pipeline entry point — later issues (#3-#7) replace the no-ops inside it
- Tool description loaded via `include_str!("tool_description.txt")` at compile time

### Daemon Mode

```rust
// In main(), when --daemon is set:
// 1. Relaunch self without --daemon flag
// 2. Redirect stdout/stderr to log file or /dev/null
// 3. Parent exits immediately
// 4. Child runs the server

if args.daemon {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .args(["start", "--port", &args.port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    return Ok(());
}
```

### Graceful Shutdown

```rust
// In run_server():
let listener = tokio::net::TcpListener::bind(addr).await?;
axum::serve(listener, router)
    .with_graceful_shutdown(shutdown_signal())
    .await?;

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(SignalKind::terminate()).unwrap();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term.recv() => {},
    }
    // TODO: flush traces, save ML model (no-op for now)
}
```

### McpState

```rust
pub struct McpState {
    pub server: ThinkingServer,
    pub sessions: RwLock<HashMap<String, Session>>,
}

pub struct Session {
    pub id: String,
    pub initialized: bool,
    pub created_at: Timestamp,
    pub last_activity: Timestamp,  // updated on every request
}

// Session TTL: background task sweeps every 60 seconds,
// evicts sessions with last_activity > 30 minutes ago.
// Spawned in run_server() via tokio::spawn.
async fn session_cleanup_task(state: Arc<McpState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let cutoff = now_millis() - 30 * 60 * 1000; // 30 min
        state.sessions.write().await.retain(|_, s| s.last_activity > cutoff);
    }
}
```

### Issues that update this code later

| Issue | What changes |
|-------|-------------|
| #3 (Analyzers) | Replace no-op `run_analyzer_pipeline()` with real pipeline |
| #4 (Warnings) | Replace no-op `generate_warnings()` with real engine |
| #5 (DB) | Wire `db_write_thought()` and `db.flush_trace()` to real SQLite |
| #6 (ML) | Wire `ml_predict()` and `ml_train()` to real PerpetualBooster |
| #7 (Trace Review) | Wire `trace_review()` to real OpenRouter call |

### Test Plan

```rust
// src/mcp.rs #[cfg(test)]

// JSON-RPC parsing
- test_parse_valid_request: valid JSON-RPC → parsed correctly
- test_parse_invalid_json: garbage input → HTTP 200 + JSON-RPC -32700
- test_parse_missing_method: JSON without method → HTTP 200 + JSON-RPC -32600
- test_unknown_method: method "foo/bar" → HTTP 200 + JSON-RPC -32601
- test_jsonrpc_error_returns_http_200: all JSON-RPC errors return HTTP 200, not 4xx

// Origin validation
- test_request_without_origin_allowed: no Origin header → request proceeds
- test_request_with_localhost_origin_allowed: Origin: http://localhost:3581 → allowed
- test_request_with_foreign_origin_rejected: Origin: http://evil.com → 403

// Initialize
- test_initialize_returns_capabilities: protocolVersion "2025-11-25", tools capability, serverInfo
- test_initialize_returns_session_id: response has Mcp-Session-Id header
- test_initialize_negotiates_version: client sends "2025-11-25" → server responds "2025-11-25"
- test_initialize_rejects_unsupported_version: client sends "1999-01-01" → JSON-RPC error
- test_initialized_notification_returns_202: notifications/initialized → 202

// Session management
- test_request_without_session_returns_400: POST without Mcp-Session-Id after init → 400
- test_request_with_invalid_session_returns_404: POST with wrong session ID → 404
- test_initialize_does_not_require_session: initialize request works without session header

// Tools
- test_tools_list_returns_sequentialthinking: tools/list → one tool with correct schema including traceId
- test_tools_call_valid_thought: valid ThoughtInput → 200 with WireResponse in content[0].text
- test_tools_call_unknown_tool: tools/call with name "foo" → -32602
- test_tools_call_invalid_args: tools/call with bad arguments → -32602
- test_tools_call_extracts_params_arguments: verify ThoughtInput parsed from params.arguments, not params

// Batch handling
- test_batch_mixed_requests_and_notifications: responses only for requests
- test_batch_all_notifications_returns_202: all notifications → 202
- test_empty_batch_returns_error: empty array → -32600

// HTTP methods
- test_get_mcp_returns_405: GET /mcp → 405
- test_delete_mcp_returns_405: DELETE /mcp → 405
- test_health_returns_ok: GET /health → 200 {"status": "ok"}

// Integration
- test_full_thought_flow: initialize → initialized → tools/call thought 1 (no trace_id) → get traceId in response → tools/call thought 2 (with trace_id) → nextThoughtNeeded=false → verify trace created and closed
- test_wire_response_has_correct_fields: verify content[0].text contains traceId, thoughtNumber, trajectory (not mlTrajectory)
```

### Risk Mitigation

| Risk | Probability | Impact | Mitigation |
|------|------------|--------|------------|
| MCP spec non-compliance | Medium | High | Test against Claude Code directly after build. Origin validation + version negotiation added. |
| Daemon mode doesn't detach properly | Low | Medium | Simple self-relaunch pattern. Noted: add setsid + log file in implementation. |
| Session memory leak | Low | Low | Session TTL (30 min) with 60-second background sweep added to design. |
| JSON-RPC batch support missed | Medium | Medium | Batch handling rules fully specified (empty, all-notifications, mixed). |
| Protocol version mismatch | Low | Medium | Version negotiation implemented. Support `2025-11-25`. |
