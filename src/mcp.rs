use crate::thought::{ThinkingServer, ThoughtInput, Timestamp};
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{delete, get, post},
    Json,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const SUPPORTED_VERSION: &str = "2025-11-25";
const SESSION_TTL_MS: i64 = 30 * 60 * 1000;
const CLEANUP_INTERVAL_SECS: u64 = 60;

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

pub async fn session_cleanup_task(state: Arc<McpState>) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    loop {
        interval.tick().await;
        let cutoff = now_millis() - SESSION_TTL_MS;
        state
            .sessions
            .write()
            .await
            .retain(|_, s| s.last_activity > cutoff);
    }
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
        Ok(())
    }
}

async fn validate_session(state: &McpState, headers: &HeaderMap) -> Result<String, StatusCode> {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    let sessions = state.sessions.read().await;
    if sessions.contains_key(session_id) {
        Ok(session_id.to_owned())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

// --- JSON-RPC types ---

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

// --- Main POST handler ---

async fn handle_post(
    State(state): State<Arc<McpState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if let Err(status) = validate_origin(&headers) {
        return (status, "").into_response();
    }

    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::OK,
                Json(jsonrpc_error(None, -32700, "Parse error")),
            )
                .into_response();
        }
    };

    // Batch
    if let Some(arr) = parsed.as_array() {
        if arr.is_empty() {
            return (
                StatusCode::OK,
                Json(jsonrpc_error(None, -32600, "Invalid request: empty batch")),
            )
                .into_response();
        }
        let mut responses = Vec::new();
        for msg in arr {
            match dispatch_message(&state, &headers, msg.clone()).await {
                DispatchResult::Response(resp, _) => responses.push(resp),
                DispatchResult::TransportError(status) => return status.into_response(),
                DispatchResult::Accepted => {}
            }
        }
        if responses.is_empty() {
            return StatusCode::ACCEPTED.into_response();
        }
        return (StatusCode::OK, Json(Value::Array(responses))).into_response();
    }

    // Single message
    // Special-case initialize to inject Mcp-Session-Id response header
    let method = parsed
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_owned();

    match dispatch_message(&state, &headers, parsed).await {
        DispatchResult::Response(resp, session_id) => {
            let mut response = (StatusCode::OK, Json(resp)).into_response();
            if method == "initialize" {
                if let Some(id) = session_id {
                    if let Ok(val) = HeaderValue::from_str(&id) {
                        response.headers_mut().insert("mcp-session-id", val);
                    }
                }
            }
            response
        }
        DispatchResult::TransportError(status) => status.into_response(),
        DispatchResult::Accepted => StatusCode::ACCEPTED.into_response(),
    }
}

enum DispatchResult {
    /// JSON-RPC response at HTTP 200 (with optional session ID for initialize)
    Response(Value, Option<String>),
    /// Notification/response — return HTTP 202
    Accepted,
    /// Transport-level error — return raw HTTP status (400, 404)
    TransportError(StatusCode),
}

async fn dispatch_message(
    state: &McpState,
    headers: &HeaderMap,
    msg: Value,
) -> DispatchResult {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str());
    let params = msg.get("params").cloned();

    // Incoming response (has result or error, no method) → ignore
    if method.is_none() && (msg.get("result").is_some() || msg.get("error").is_some()) {
        return DispatchResult::Accepted;
    }

    let method = match method {
        Some(m) => m,
        None => {
            return if id.is_some() {
                DispatchResult::Response(
                    jsonrpc_error(id, -32600, "Invalid request: missing method"),
                    None,
                )
            } else {
                DispatchResult::Accepted
            };
        }
    };

    // Notifications (no id) — handle and return Accepted
    if id.is_none() {
        return DispatchResult::Accepted;
    }

    let id = id.unwrap();

    // Session validation (skip for initialize)
    if method != "initialize" {
        match validate_session(state, headers).await {
            Ok(session_id) => {
                if let Some(session) = state.sessions.write().await.get_mut(&session_id) {
                    session.last_activity = now_millis();
                }
            }
            Err(status) => {
                // Transport-level error: HTTP 400 (missing session) or 404 (unknown session)
                return DispatchResult::TransportError(status);
            }
        }
    }

    match method {
        "initialize" => {
            let (response, session_id) = handle_initialize(state, id, params).await;
            DispatchResult::Response(response, Some(session_id))
        }
        "tools/list" => DispatchResult::Response(handle_tools_list(id), None),
        "tools/call" => DispatchResult::Response(handle_tools_call(state, id, params).await, None),
        "ping" => DispatchResult::Response(jsonrpc_result(id, json!({})), None),
        _ => DispatchResult::Response(
            jsonrpc_error(Some(id), -32601, &format!("Method not found: {}", method)),
            None,
        ),
    }
}

async fn handle_initialize(
    state: &McpState,
    id: Value,
    params: Option<Value>,
) -> (Value, String) {
    if let Some(ref p) = params {
        if let Some(version) = p.get("protocolVersion").and_then(|v| v.as_str()) {
            if version != SUPPORTED_VERSION {
                let err = jsonrpc_error(
                    Some(id),
                    -32602,
                    &format!(
                        "Unsupported protocol version: {}. Supported: {}",
                        version, SUPPORTED_VERSION
                    ),
                );
                return (err, String::new());
            }
        }
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let now = now_millis();
    state.sessions.write().await.insert(
        session_id.clone(),
        Session {
            id: session_id.clone(),
            initialized: true,
            created_at: now,
            last_activity: now,
        },
    );

    let response = jsonrpc_result(
        id,
        json!({
            "protocolVersion": SUPPORTED_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "feldspar",
                "version": "0.1.0"
            }
        }),
    );
    (response, session_id)
}

fn handle_tools_list(id: Value) -> Value {
    jsonrpc_result(
        id,
        json!({
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
        }),
    )
}

async fn handle_tools_call(state: &McpState, id: Value, params: Option<Value>) -> Value {
    let params = match params {
        Some(p) => p,
        None => return jsonrpc_error(Some(id), -32602, "Missing params"),
    };

    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if name != "sequentialthinking" {
        return jsonrpc_error(Some(id), -32602, &format!("Unknown tool: {}", name));
    }

    let arguments = match params.get("arguments") {
        Some(args) => args,
        None => return jsonrpc_error(Some(id), -32602, "Missing arguments"),
    };

    let input: ThoughtInput = match serde_json::from_value(arguments.clone()) {
        Ok(i) => i,
        Err(e) => return jsonrpc_error(Some(id), -32602, &format!("Invalid arguments: {}", e)),
    };

    match state.server.process_thought(input).await {
        Ok(wire) => {
            let text = serde_json::to_string(&wire).unwrap();
            jsonrpc_result(
                id,
                json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": false
                }),
            )
        }
        Err(e) => jsonrpc_result(
            id,
            json!({
                "content": [{"type": "text", "text": e}],
                "isError": true
            }),
        ),
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::collections::HashMap;
    use tower::util::ServiceExt;

    fn test_config() -> Arc<crate::config::Config> {
        Arc::new(crate::config::Config {
            feldspar: crate::config::FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
            },
            llm: crate::config::LlmConfig {
                base_url: None,
                api_key_env: Some("TEST_KEY".into()),
                model: "test-model".into(),
            },
            thresholds: crate::config::ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([
                ("minimal".into(), [2, 3]),
                ("standard".into(), [3, 5]),
                ("deep".into(), [5, 8]),
            ]),
            modes: HashMap::from([(
                "architecture".into(),
                crate::config::ModeConfig {
                    requires: vec![],
                    budget: "deep".into(),
                    watches: "test".into(),
                },
            )]),
            components: crate::config::ComponentsConfig { valid: vec![] },
            principles: vec![],
        })
    }

    fn test_app() -> Router<()> {
        use std::collections::HashMap;
        use tokio::sync::RwLock;
        let config = test_config();
        let server = ThinkingServer::new(
            config,
            None,
            None,
            Arc::new(RwLock::new(HashMap::new())),
        );
        let state = Arc::new(McpState::new(server));
        create_router(state)
    }

    async fn post_mcp(app: Router<()>, body: &str) -> axum::response::Response<Body> {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn post_mcp_with_session(
        app: Router<()>,
        body: &str,
        session_id: &str,
    ) -> axum::response::Response<Body> {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("mcp-session-id", session_id)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn body_json(resp: axum::response::Response<Body>) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    // Helper: do full initialize, return (app, session_id)
    async fn initialized_app() -> (Router<()>, String) {
        let app = test_app();
        let resp: axum::response::Response<Body> = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let session_id = resp
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();

        (app, session_id)
    }

    // --- Origin tests (Task 2) ---

    #[test]
    fn test_origin_no_header_allowed() {
        assert!(validate_origin(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn test_origin_localhost_allowed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://localhost:3581".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());
    }

    #[test]
    fn test_origin_127_allowed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://127.0.0.1:3581".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());
    }

    #[test]
    fn test_origin_foreign_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://evil.com".parse().unwrap());
        assert_eq!(validate_origin(&headers), Err(StatusCode::FORBIDDEN));
    }

    #[test]
    fn test_origin_null_allowed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());
    }

    // --- Basic route tests (Task 3) ---

    #[tokio::test]
    async fn test_health_returns_ok() {
        let app = test_app();
        let resp: axum::response::Response<Body> = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn test_get_mcp_returns_405() {
        let app = test_app();
        let resp: axum::response::Response<Body> = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_delete_mcp_returns_405() {
        let app = test_app();
        let resp: axum::response::Response<Body> = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_parse_invalid_json() {
        let resp = post_mcp(test_app(), "not json{{").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"foo/bar"}"#,
            &session_id,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn test_jsonrpc_error_returns_http_200() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"bad"}"#,
            &session_id,
        )
        .await;
        // JSON-RPC protocol errors (method not found) return HTTP 200, not 4xx
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32601);
    }

    // --- Initialize + session tests (Task 4) ---

    #[tokio::test]
    async fn test_initialize_returns_capabilities() {
        let app = test_app();
        let resp = post_mcp(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
        assert!(body["result"]["capabilities"]["tools"].is_object());
        assert_eq!(body["result"]["serverInfo"]["name"], "feldspar");
    }

    #[tokio::test]
    async fn test_initialize_returns_session_header() {
        let app = test_app();
        let resp = post_mcp(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#,
        )
        .await;
        let session_id = resp.headers().get("mcp-session-id");
        assert!(session_id.is_some());
        let id_str = session_id.unwrap().to_str().unwrap();
        // UUID format: 36 chars
        assert_eq!(id_str.len(), 36);
    }

    #[tokio::test]
    async fn test_initialize_rejects_bad_version() {
        let app = test_app();
        let resp = post_mcp(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        )
        .await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_some());
    }

    #[tokio::test]
    async fn test_initialized_notification_returns_202() {
        let app = test_app();
        let resp = post_mcp(
            app,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_tools_list_has_sequentialthinking() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &session_id,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "sequentialthinking");
        assert!(tools[0]["inputSchema"]["properties"]["traceId"].is_object());
    }

    #[tokio::test]
    async fn test_tools_call_valid_thought() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"test","thoughtNumber":1,"totalThoughts":3,"nextThoughtNeeded":true}}}"#,
            &session_id,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(!body["result"]["isError"].as_bool().unwrap_or(true));
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let wire: Value = serde_json::from_str(text).unwrap();
        assert!(wire["traceId"].is_string());
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"foo","arguments":{}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_tools_call_invalid_args() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"bad":"data"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_tools_call_extracts_params_arguments() {
        let (app, session_id) = initialized_app().await;
        // arguments are inside params.arguments, not top-level
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"hello","thoughtNumber":1,"totalThoughts":1,"nextThoughtNeeded":false}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert!(!body["result"]["isError"].as_bool().unwrap_or(true));
    }

    #[tokio::test]
    async fn test_request_without_session_returns_400() {
        let app = test_app();
        let resp = post_mcp(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_request_with_invalid_session_returns_404() {
        let app = test_app();
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "bogus-session-id",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- Batch tests (Task 5) ---

    #[tokio::test]
    async fn test_batch_mixed() {
        let (app, session_id) = initialized_app().await;
        // 1 request (ping) + 1 notification
        let body = format!(
            r#"[{{"jsonrpc":"2.0","id":10,"method":"ping"}},{{"jsonrpc":"2.0","method":"notifications/initialized"}}]"#
        );
        let resp = post_mcp_with_session(app, &body, &session_id).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let parsed = body_json(resp).await;
        let arr = parsed.as_array().unwrap();
        // Only ping request gets a response (notification has no id → None)
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], 10);
    }

    #[tokio::test]
    async fn test_batch_all_notifications_returns_202() {
        let app = test_app();
        let body = r#"[{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","method":"notifications/foo"}]"#;
        let resp = post_mcp(app, body).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_empty_batch_returns_error() {
        let resp = post_mcp(test_app(), "[]").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn test_full_thought_flow() {
        let app = test_app();

        // 1. Initialize
        let resp: axum::response::Response<Body> = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let session_id = resp
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(resp.status(), StatusCode::OK);

        // 2. notifications/initialized
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &session_id,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        // 3. tools/call thought 1 (no traceId)
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"first thought","thoughtNumber":1,"totalThoughts":2,"nextThoughtNeeded":true}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let wire: Value = serde_json::from_str(text).unwrap();
        let trace_id = wire["traceId"].as_str().unwrap().to_owned();
        assert_eq!(wire["thoughtNumber"], 1);

        // 4. tools/call thought 2 (with traceId)
        let call2 = format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"sequentialthinking","arguments":{{"traceId":"{trace_id}","thought":"second thought","thoughtNumber":2,"totalThoughts":2,"nextThoughtNeeded":false}}}}}}"#
        );
        let resp = post_mcp_with_session(app.clone(), &call2, &session_id).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let wire2: Value = serde_json::from_str(text).unwrap();
        assert_eq!(wire2["thoughtHistoryLength"], 2);
    }

    #[tokio::test]
    async fn test_wire_response_has_correct_fields() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"check","thoughtNumber":1,"totalThoughts":1,"nextThoughtNeeded":false}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let wire: Value = serde_json::from_str(text).unwrap();

        assert!(wire["traceId"].is_string());
        assert!(wire["thoughtNumber"].is_number());
        assert!(wire["budgetCategory"].is_string());
        // Correct field names used (not ml_ prefixed)
        assert!(wire.get("mlTrajectory").is_none());
        assert!(wire.get("mlDrift").is_none());
        // None fields are omitted (skip_serializing_if), not present as null
        assert!(wire.get("trajectory").is_none()); // None → omitted
        assert!(wire.get("driftDetected").is_none()); // None → omitted
        assert!(wire.get("biasDetected").is_none()); // None → omitted
    }
}
