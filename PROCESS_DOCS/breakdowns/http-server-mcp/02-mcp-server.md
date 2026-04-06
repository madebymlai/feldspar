# Build Agent 2: MCP Server + CLI

## Dependencies

**01-types-pipeline.md must complete first.** This agent imports `ThoughtInput`, `WireResponse`, `ThinkingServer`, and `ThinkingServer::process_thought()` from `src/thought.rs`.

## Overview

- **Objective**: Implement the HTTP server, MCP streamable HTTP transport, JSON-RPC dispatch, CLI, and session management. This is the transport layer connecting Claude Code to feldspar.
- **Scope**:
  - Includes: `src/main.rs` (CLI + server), `src/mcp.rs` (new — MCP protocol), `Cargo.toml` (add clap)
  - Excludes: No changes to `src/thought.rs` (done by Agent 1), no analyzer/ML/DB logic
- **Dependencies**:
  - Agent 1 output: `ThoughtInput`, `ThoughtResult`, `WireResponse`, `ThinkingServer`, `Trace`, `Timestamp`, `Alert`, `Severity` from `src/thought.rs`
  - `Config` from `src/config.rs`
  - Crates: `axum` 0.8, `tokio` 1, `serde`/`serde_json` 1, `uuid` 1, `clap` 4 (new)
- **Estimated Complexity**: Medium-High — MCP spec compliance, JSON-RPC dispatch, session management, Origin validation

## Technical Approach

### Architecture

```
main.rs: CLI (clap) → run_server() → axum router → graceful shutdown
mcp.rs:  McpState → Origin middleware → POST /mcp → JSON-RPC dispatch → method handlers
```

### Key Design Decisions

- **All JSON-RPC responses return HTTP 200** — errors go in the body, not HTTP status
- **HTTP 4xx only for transport-level issues** — 400 (missing session), 403 (bad Origin), 404 (expired session), 405 (wrong HTTP method)
- **Origin validation** on all requests — MCP spec MUST
- **Protocol version negotiation** — support `2025-11-25`, reject unsupported
- **Session TTL** — 30 min inactivity, 60-second background sweep
- **`application/json` responses** — no SSE (pipeline is microsecond-fast)
- **GET /mcp returns 405** — no server-initiated messages

### Module Placement

```
src/main.rs  ← CLI struct (clap), main(), run_server(), shutdown_signal()
src/mcp.rs   ← McpState, Session, create_router(), all MCP handlers, Origin middleware
Cargo.toml   ← add clap
```

---

## Task Breakdown

### Task 1: Add clap dependency and implement CLI in main.rs

- **Description**: Replace the existing `src/main.rs` stub with CLI parsing via clap and server startup logic.
- **Acceptance Criteria**:
  - [ ] `clap = { version = "4", features = ["derive"] }` added to Cargo.toml
  - [ ] `feldspar start` starts the server on port 3581
  - [ ] `feldspar start --port 4000` starts on port 4000
  - [ ] `feldspar start --daemon` spawns background process and exits
  - [ ] Server binds to `127.0.0.1` (localhost only)
  - [ ] Graceful shutdown on SIGINT/SIGTERM
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  Cargo.toml    ← add clap
  src/main.rs   ← replace stub entirely
  ```
- **Dependencies**: None (but mcp.rs must exist for `mod mcp;` — create empty file first)
- **Code — Cargo.toml addition**:
  ```toml
  # CLI
  clap = { version = "4", features = ["derive"] }
  ```
- **Code — main.rs**:
  ```rust
  mod analyzers;
  mod config;
  mod db;
  mod mcp;
  mod ml;
  mod pruning;
  mod thought;
  mod trace_review;
  mod warnings;

  use clap::{Parser, Subcommand};
  use std::sync::Arc;
  use tracing::info;

  #[derive(Parser)]
  #[command(name = "feldspar", about = "Cognitive reasoning MCP server")]
  struct Cli {
      #[command(subcommand)]
      command: Commands,
  }

  #[derive(Subcommand)]
  enum Commands {
      /// Start the feldspar MCP server
      Start {
          /// Run as background daemon
          #[arg(long)]
          daemon: bool,
          /// Port to listen on
          #[arg(long, default_value = "3581")]
          port: u16,
      },
  }

  #[tokio::main]
  async fn main() {
      tracing_subscriber::fmt()
          .with_target(false)
          .json()
          .with_writer(std::io::stderr)
          .init();

      let cli = Cli::parse();

      match cli.command {
          Commands::Start { daemon, port } => {
              if daemon {
                  let exe = std::env::current_exe().expect("failed to get executable path");
                  std::process::Command::new(exe)
                      .args(["start", "--port", &port.to_string()])
                      .stdin(std::process::Stdio::null())
                      .stdout(std::process::Stdio::null())
                      .stderr(std::process::Stdio::null())
                      .spawn()
                      .expect("failed to spawn daemon");
                  println!("feldspar daemon started on port {}", port);
                  return;
              }

              run_server(port).await;
          }
      }
  }

  async fn run_server(port: u16) {
      let config = config::Config::load("config/feldspar.toml", "config/principles.toml");
      let server = thought::ThinkingServer::new(config);
      let state = Arc::new(mcp::McpState::new(server));

      // Spawn session cleanup background task
      let cleanup_state = state.clone();
      tokio::spawn(mcp::session_cleanup_task(cleanup_state));

      let router = mcp::create_router(state);
      let addr = format!("127.0.0.1:{}", port);
      info!("feldspar listening on {}", addr);

      let listener = tokio::net::TcpListener::bind(&addr)
          .await
          .unwrap_or_else(|e| panic!("failed to bind to {}: {}", addr, e));

      axum::serve(listener, router)
          .with_graceful_shutdown(shutdown_signal())
          .await
          .expect("server error");

      info!("feldspar shutdown complete");
  }

  async fn shutdown_signal() {
      let ctrl_c = tokio::signal::ctrl_c();
      #[cfg(unix)]
      {
          let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
              .expect("failed to install SIGTERM handler");
          tokio::select! {
              _ = ctrl_c => {},
              _ = term.recv() => {},
          }
      }
      #[cfg(not(unix))]
      {
          ctrl_c.await.ok();
      }
      info!("shutdown signal received");
  }
  ```
- **Test Cases**: CLI is tested manually and via integration tests in Task 5. No unit tests for main.rs.

---

### Task 2: Implement MCP state, session management, and Origin middleware in mcp.rs

- **Description**: Create `src/mcp.rs` with `McpState`, `Session`, Origin validation, session management, and the `create_router()` entry point.
- **Acceptance Criteria**:
  - [ ] `McpState` holds ThinkingServer + sessions HashMap
  - [ ] `Session` has id, initialized, created_at, last_activity
  - [ ] `validate_origin()` rejects foreign Origin headers with 403
  - [ ] `validate_session()` checks Mcp-Session-Id header (400 if missing, 404 if unknown)
  - [ ] `create_router()` returns axum Router with all routes
  - [ ] `session_cleanup_task()` evicts sessions idle > 30 min
  - [ ] `cargo check` passes
- **Files to Create**:
  ```
  src/mcp.rs   ← NEW
  ```
- **Dependencies**: Task 1 (main.rs must have `mod mcp;`)
- **Code — Core structures and router**:
  ```rust
  use crate::config::Config;
  use crate::thought::{ThinkingServer, ThoughtInput, WireResponse, Timestamp};
  use axum::{
      Router,
      routing::{get, post, delete},
      extract::State,
      http::{StatusCode, HeaderMap, header},
      response::IntoResponse,
      Json,
      body::Body,
  };
  use serde::{Deserialize, Serialize};
  use serde_json::{json, Value};
  use std::collections::HashMap;
  use std::sync::Arc;
  use tokio::sync::RwLock;

  pub struct McpState {
      pub server: ThinkingServer,
      pub sessions: RwLock<HashMap<String, Session>>,
  }

  pub struct Session {
      pub id: String,
      pub initialized: bool,
      pub created_at: Timestamp,
      pub last_activity: Timestamp,
  }

  impl McpState {
      pub fn new(server: ThinkingServer) -> Self {
          Self {
              server,
              sessions: RwLock::new(HashMap::new()),
          }
      }
  }

  fn now_millis() -> Timestamp {
      std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap()
          .as_millis() as i64
  }

  pub fn create_router(state: Arc<McpState>) -> Router {
      Router::new()
          .route("/health", get(handle_health))
          .route("/mcp", post(handle_post))
          .route("/mcp", get(handle_get))
          .route("/mcp", delete(handle_delete))
          .with_state(state)
  }

  async fn handle_health() -> impl IntoResponse {
      Json(json!({"status": "ok"}))
  }

  async fn handle_get() -> impl IntoResponse {
      StatusCode::METHOD_NOT_ALLOWED
  }

  async fn handle_delete() -> impl IntoResponse {
      StatusCode::METHOD_NOT_ALLOWED
  }

  pub async fn session_cleanup_task(state: Arc<McpState>) {
      let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
      loop {
          interval.tick().await;
          let cutoff = now_millis() - 30 * 60 * 1000;
          state.sessions.write().await.retain(|_, s| s.last_activity > cutoff);
      }
  }
  ```
- **Code — Origin validation** (implement as a check within `handle_post`, not a separate middleware layer, for simplicity):
  ```rust
  fn validate_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
      if let Some(origin) = headers.get(header::ORIGIN) {
          let origin_str = origin.to_str().unwrap_or("");
          if origin_str.starts_with("http://localhost")
              || origin_str.starts_with("http://127.0.0.1")
              || origin_str == "null"
          {
              Ok(())
          } else {
              Err(StatusCode::FORBIDDEN)
          }
      } else {
          // No Origin header — non-browser client (curl, Claude Code). Allow.
          Ok(())
      }
  }
  ```
- **Test Cases** (file: `src/mcp.rs` inline `#[cfg(test)]`):
  - **`test_origin_no_header_allowed`**: `validate_origin(&HeaderMap::new())` → `Ok(())`
  - **`test_origin_localhost_allowed`**: HeaderMap with `Origin: http://localhost:3581` → `Ok(())`
  - **`test_origin_127_allowed`**: HeaderMap with `Origin: http://127.0.0.1:3581` → `Ok(())`
  - **`test_origin_foreign_rejected`**: HeaderMap with `Origin: http://evil.com` → `Err(FORBIDDEN)`
  - **`test_origin_null_allowed`**: HeaderMap with `Origin: null` → `Ok(())`

---

### Task 3: Implement JSON-RPC dispatch in handle_post

- **Description**: The core POST /mcp handler. Parses JSON-RPC messages, validates Origin and session, dispatches to method handlers.
- **Acceptance Criteria**:
  - [ ] Parses single JSON-RPC messages and batches
  - [ ] `initialize` method: negotiates version, returns capabilities, sets `Mcp-Session-Id` header
  - [ ] `notifications/initialized` returns 202
  - [ ] `tools/list` returns tool catalog with sequentialthinking
  - [ ] `tools/call` validates `params.name`, deserializes `params.arguments`, calls `process_thought()`
  - [ ] `ping` returns empty result
  - [ ] Unknown method returns JSON-RPC -32601
  - [ ] Invalid JSON returns JSON-RPC -32700
  - [ ] All JSON-RPC responses return HTTP 200
  - [ ] Notifications return HTTP 202
  - [ ] Session validated on all requests after initialize
- **Files to Modify**:
  ```
  src/mcp.rs
  ```
- **Dependencies**: Task 2
- **Code — JSON-RPC types**:
  ```rust
  #[derive(Debug, Deserialize)]
  struct JsonRpcRequest {
      jsonrpc: Option<String>,
      id: Option<Value>,
      method: Option<String>,
      params: Option<Value>,
      // For responses (result/error) — we just return 202
      result: Option<Value>,
      error: Option<Value>,
  }

  fn jsonrpc_error(id: Option<Value>, code: i32, message: &str) -> Value {
      json!({
          "jsonrpc": "2.0",
          "id": id,
          "error": {
              "code": code,
              "message": message
          }
      })
  }

  fn jsonrpc_result(id: Value, result: Value) -> Value {
      json!({
          "jsonrpc": "2.0",
          "id": id,
          "result": result
      })
  }
  ```
- **Code — handle_post skeleton**:
  ```rust
  async fn handle_post(
      State(state): State<Arc<McpState>>,
      headers: HeaderMap,
      body: String,
  ) -> impl IntoResponse {
      // 1. Validate Origin
      if let Err(status) = validate_origin(&headers) {
          return (status, "").into_response();
      }

      // 2. Parse JSON
      let parsed: Value = match serde_json::from_str(&body) {
          Ok(v) => v,
          Err(_) => {
              return (StatusCode::OK, Json(jsonrpc_error(None, -32700, "Parse error"))).into_response();
          }
      };

      // 3. Handle batch vs single
      if let Some(arr) = parsed.as_array() {
          if arr.is_empty() {
              return (StatusCode::OK, Json(jsonrpc_error(None, -32600, "Invalid request: empty batch"))).into_response();
          }
          let mut responses = Vec::new();
          for msg in arr {
              if let Some(resp) = dispatch_message(&state, &headers, msg.clone()).await {
                  responses.push(resp);
              }
          }
          if responses.is_empty() {
              return StatusCode::ACCEPTED.into_response();
          }
          return (StatusCode::OK, Json(Value::Array(responses))).into_response();
      }

      // 4. Single message
      if let Some(resp) = dispatch_message(&state, &headers, parsed).await {
          (StatusCode::OK, Json(resp)).into_response()
      } else {
          StatusCode::ACCEPTED.into_response()
      }
  }
  ```
- **Code — dispatch_message**:
  ```rust
  /// Returns None for notifications/responses (202), Some(response) for requests.
  async fn dispatch_message(state: &McpState, headers: &HeaderMap, msg: Value) -> Option<Value> {
      let id = msg.get("id").cloned();
      let method = msg.get("method").and_then(|m| m.as_str());
      let params = msg.get("params").cloned();

      // If it's a response (has result or error, no method), return None → 202
      if method.is_none() && (msg.get("result").is_some() || msg.get("error").is_some()) {
          return None;
      }

      let method = match method {
          Some(m) => m,
          None => {
              // No method and no result/error → invalid
              return if id.is_some() {
                  Some(jsonrpc_error(id, -32600, "Invalid request: missing method"))
              } else {
                  None // notification without method — ignore
              };
          }
      };

      // If no id, it's a notification — handle and return None
      if id.is_none() {
          // Handle notifications (notifications/initialized, etc.)
          return None;
      }

      let id = id.unwrap();

      // Session validation (skip for initialize)
      if method != "initialize" {
          if let Err(status) = validate_session(state, headers).await {
              return Some(jsonrpc_error(Some(id), -32600,
                  &format!("Session error: {}", status.as_u16())));
          }
          // Update last_activity
          if let Some(session_id) = headers.get("mcp-session-id").and_then(|h| h.to_str().ok()) {
              if let Some(session) = state.sessions.write().await.get_mut(session_id) {
                  session.last_activity = now_millis();
              }
          }
      }

      match method {
          "initialize" => Some(handle_initialize(state, id, params).await),
          "tools/list" => Some(handle_tools_list(id)),
          "tools/call" => Some(handle_tools_call(state, id, params).await),
          "ping" => Some(jsonrpc_result(id, json!({}))),
          _ => Some(jsonrpc_error(Some(id), -32601, &format!("Method not found: {}", method))),
      }
  }
  ```
- **Code — initialize handler**:
  ```rust
  const SUPPORTED_VERSION: &str = "2025-11-25";

  async fn handle_initialize(state: &McpState, id: Value, params: Option<Value>) -> Value {
      // Version negotiation
      if let Some(ref p) = params {
          if let Some(version) = p.get("protocolVersion").and_then(|v| v.as_str()) {
              if version != SUPPORTED_VERSION {
                  return jsonrpc_error(Some(id), -32602, &format!(
                      "Unsupported protocol version: {}. Supported: {}", version, SUPPORTED_VERSION
                  ));
              }
          }
      }

      let session_id = uuid::Uuid::new_v4().to_string();
      let now = now_millis();
      state.sessions.write().await.insert(session_id.clone(), Session {
          id: session_id.clone(),
          initialized: true,
          created_at: now,
          last_activity: now,
      });

      // NOTE: The session ID must be returned as an HTTP header (Mcp-Session-Id).
      // This is handled by wrapping the response in handle_post — see implementation note below.
      jsonrpc_result(id, json!({
          "protocolVersion": SUPPORTED_VERSION,
          "capabilities": {
              "tools": {}
          },
          "serverInfo": {
              "name": "feldspar",
              "version": "0.1.0"
          }
      }))
  }
  ```

  **Implementation note**: The `Mcp-Session-Id` header on the initialize response requires special handling in `handle_post`. After `dispatch_message` returns the initialize result, check if the method was "initialize" and if successful, extract the session ID and add it as a response header. One approach:
  ```rust
  // In handle_post, after getting the response for a single initialize request:
  // Build an axum Response with the Mcp-Session-Id header
  ```
- **Code — tools/list handler**:
  ```rust
  fn handle_tools_list(id: Value) -> Value {
      jsonrpc_result(id, json!({
          "tools": [{
              "name": "sequentialthinking",
              "description": include_str!("tool_description.txt"),
              "inputSchema": {
                  "type": "object",
                  "properties": {
                      "traceId": { "type": "string", "description": "Trace ID returned from thought 1. Required for thought 2+." },
                      "thought": { "type": "string", "description": "Your current reasoning step." },
                      "thoughtNumber": { "type": "integer", "description": "Current step (1-indexed)." },
                      "totalThoughts": { "type": "integer", "description": "Estimated total." },
                      "nextThoughtNeeded": { "type": "boolean", "description": "True if more thinking needed." },
                      "thinkingMode": { "type": "string", "description": "Domain mode (architecture, debugging, etc)." },
                      "affectedComponents": { "type": "array", "items": { "type": "string" } },
                      "confidence": { "type": "number", "description": "Self-reported confidence 0-100." },
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
          }]
      }))
  }
  ```
- **Code — tools/call handler**:
  ```rust
  async fn handle_tools_call(state: &McpState, id: Value, params: Option<Value>) -> Value {
      let params = match params {
          Some(p) => p,
          None => return jsonrpc_error(Some(id), -32602, "Missing params"),
      };

      // Validate tool name
      let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
      if name != "sequentialthinking" {
          return jsonrpc_error(Some(id), -32602, &format!("Unknown tool: {}", name));
      }

      // Deserialize params.arguments as ThoughtInput
      let arguments = match params.get("arguments") {
          Some(args) => args,
          None => return jsonrpc_error(Some(id), -32602, "Missing arguments"),
      };

      let input: ThoughtInput = match serde_json::from_value(arguments.clone()) {
          Ok(i) => i,
          Err(e) => return jsonrpc_error(Some(id), -32602, &format!("Invalid arguments: {}", e)),
      };

      // Process thought
      match state.server.process_thought(input).await {
          Ok(wire) => {
              let text = serde_json::to_string(&wire).unwrap();
              jsonrpc_result(id, json!({
                  "content": [{"type": "text", "text": text}],
                  "isError": false
              }))
          }
          Err(e) => {
              jsonrpc_result(id, json!({
                  "content": [{"type": "text", "text": e}],
                  "isError": true
              }))
          }
      }
  }
  ```
- **Code — session validation**:
  ```rust
  async fn validate_session(state: &McpState, headers: &HeaderMap) -> Result<(), StatusCode> {
      let session_id = headers
          .get("mcp-session-id")
          .and_then(|h| h.to_str().ok())
          .ok_or(StatusCode::BAD_REQUEST)?;

      let sessions = state.sessions.read().await;
      if sessions.contains_key(session_id) {
          Ok(())
      } else {
          Err(StatusCode::NOT_FOUND)
      }
  }
  ```
- **Test Cases** (file: `src/mcp.rs` `#[cfg(test)]`):
  - **`test_health_returns_ok`**: Send GET /health → 200, body contains `"status": "ok"`
  - **`test_get_mcp_returns_405`**: Send GET /mcp → 405
  - **`test_delete_mcp_returns_405`**: Send DELETE /mcp → 405
  - **`test_parse_invalid_json`**: POST /mcp with `"not json{{"` → 200 + JSON-RPC error -32700
  - **`test_unknown_method`**: POST valid JSON-RPC with `method: "foo/bar"` → 200 + error -32601
  - **`test_jsonrpc_error_returns_http_200`**: All error responses use HTTP 200, not 4xx

  Use `axum::test::TestClient` or build requests manually with `tower::ServiceExt::oneshot`:
  ```rust
  use axum::body::Body;
  use axum::http::Request;
  use tower::ServiceExt;

  async fn test_app() -> Router {
      let config = Config::load("config/feldspar.toml", "config/principles.toml");
      let server = ThinkingServer::new(config);
      let state = Arc::new(McpState::new(server));
      create_router(state)
  }
  ```

---

### Task 4: Implement initialize + tools handlers and session flow

- **Description**: Wire up the full initialize → tools/list → tools/call flow with session management.
- **Acceptance Criteria**:
  - [ ] `initialize` returns protocolVersion `2025-11-25`, tools capability, serverInfo
  - [ ] `initialize` response has `Mcp-Session-Id` header
  - [ ] `initialize` rejects unsupported protocol version
  - [ ] `tools/list` returns sequentialthinking with correct schema including traceId
  - [ ] `tools/call` with valid thought → 200 with WireResponse in content[0].text
  - [ ] `tools/call` with unknown tool → -32602
  - [ ] `tools/call` extracts from `params.arguments`, not `params`
  - [ ] Requests without `Mcp-Session-Id` after init → 400
  - [ ] Requests with wrong session ID → 404
- **Files to Modify**:
  ```
  src/mcp.rs
  ```
- **Dependencies**: Tasks 2, 3
- **Test Cases** (file: `src/mcp.rs`):
  - **`test_initialize_returns_capabilities`**: POST initialize request → 200, result has `protocolVersion: "2025-11-25"`, `capabilities.tools`, `serverInfo.name: "feldspar"`
  - **`test_initialize_returns_session_header`**: POST initialize → response has `Mcp-Session-Id` header with UUID
  - **`test_initialize_rejects_bad_version`**: POST initialize with `protocolVersion: "1999-01-01"` → JSON-RPC error
  - **`test_initialized_notification_returns_202`**: POST `{"jsonrpc":"2.0","method":"notifications/initialized"}` → 202
  - **`test_tools_list_has_sequentialthinking`**: POST tools/list with session → result.tools[0].name == "sequentialthinking", schema has traceId property
  - **`test_tools_call_valid_thought`**: POST tools/call with valid ThoughtInput → 200, result.content[0].text is valid JSON with traceId
  - **`test_tools_call_unknown_tool`**: POST tools/call with `name: "foo"` → -32602
  - **`test_tools_call_invalid_args`**: POST tools/call with bad arguments → -32602
  - **`test_tools_call_extracts_params_arguments`**: POST tools/call where arguments are in `params.arguments` (not top-level) → succeeds
  - **`test_request_without_session_returns_400`**: POST tools/list without Mcp-Session-Id → 400 (or JSON-RPC error with session message)
  - **`test_request_with_invalid_session_returns_404`**: POST tools/list with `Mcp-Session-Id: bogus` → 404

---

### Task 5: Implement batch handling and integration tests

- **Description**: Handle JSON-RPC batches correctly and write end-to-end integration tests.
- **Acceptance Criteria**:
  - [ ] Batch of mixed requests + notifications → response array contains only request responses
  - [ ] Batch of all notifications → 202
  - [ ] Empty batch `[]` → -32600 error
  - [ ] Full thought flow integration test passes
- **Files to Modify**:
  ```
  src/mcp.rs
  ```
- **Dependencies**: Tasks 3, 4
- **Test Cases** (file: `src/mcp.rs`):
  - **`test_batch_mixed`**: POST batch with 1 request (ping) + 1 notification → response array has 1 item (ping response)
  - **`test_batch_all_notifications_returns_202`**: POST batch of 2 notifications → 202
  - **`test_empty_batch_returns_error`**: POST `[]` → 200 + JSON-RPC -32600
  - **`test_full_thought_flow`**: Initialize → get session ID from header → POST notifications/initialized → POST tools/call thought 1 (no traceId) → parse traceId from response → POST tools/call thought 2 (with traceId, nextThoughtNeeded=false) → verify response has `thoughtHistoryLength: 2`
  - **`test_wire_response_has_correct_fields`**: After tools/call, parse content[0].text as JSON. Verify it has `traceId`, `thoughtNumber`, `trajectory` (not `mlTrajectory`), `driftDetected` (not `mlDrift`), `budgetCategory`.

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]`, `#[tokio::test]`, axum test utilities (`tower::ServiceExt::oneshot`)
- **Structure**: Tests inline in `src/mcp.rs`
- **Coverage**: ~22 tests covering Origin, JSON-RPC parsing, initialize, session, tools, batch, integration
- **Run**: `cargo test --lib mcp`
- **Helper**: `test_app()` function builds a full axum Router with real Config + ThinkingServer for integration testing

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| axum route conflicts (POST + GET + DELETE on /mcp) | Low | High | axum 0.8 supports multiple methods on same path via separate `.route()` calls | Compile error if wrong |
| Mcp-Session-Id header not passed through | Medium | High | Test explicitly for header presence in initialize response | `test_initialize_returns_session_header` |
| JSON-RPC batch ordering wrong | Low | Medium | Don't guarantee order — JSON-RPC spec says order doesn't matter | `test_batch_mixed` |
| `include_str!` path wrong | Low | High | File must be at `src/tool_description.txt` (relative to source file) | Compile error |
| Session validation races | Low | Medium | Single RwLock, read lock for validation, write lock for creation | Tests exercise concurrent access patterns |

## Success Criteria

- [ ] `feldspar start` starts server on 127.0.0.1:3581
- [ ] `feldspar start --daemon` backgrounds and exits
- [ ] GET /health → 200
- [ ] Full MCP flow: initialize → tools/list → tools/call → multi-thought trace
- [ ] Origin validation rejects foreign origins
- [ ] Session management works (create, validate, reject invalid)
- [ ] All ~22 tests pass
- [ ] No clippy warnings

## Implementation Notes

- **axum 0.8 routing**: Use `Router::new().route("/mcp", post(handle_post)).route("/mcp", get(handle_get))` — axum 0.8 allows multiple method handlers on the same path via separate `.route()` calls. If this causes issues, use `.route("/mcp", axum::routing::MethodRouter::new().post(handle_post).get(handle_get).delete(handle_delete))` instead.
- **Mcp-Session-Id header on initialize response**: The `handle_initialize` function returns a JSON-RPC Value. To add the header, `handle_post` needs special logic for initialize: after getting the response, if the method was "initialize" and response is successful, extract session ID and build an axum Response with the header. Use `axum::response::Response::builder()`.
- **`include_str!("tool_description.txt")`**: The path is relative to the source file (`src/mcp.rs`). The file `src/tool_description.txt` already exists.
- **Other module stubs**: `src/main.rs` declares `mod analyzers; mod db; mod ml; mod pruning; mod trace_review; mod warnings;` — these are still comment stubs. Don't touch them.
- **Don't modify `src/thought.rs` or `src/config.rs`** — Agent 1 handles thought.rs. config.rs is unchanged.
- **Body extraction**: Use `String` extractor for the request body (not `Json`) because we need to handle parse errors ourselves as JSON-RPC errors. axum's `Json` extractor would return its own error format.
