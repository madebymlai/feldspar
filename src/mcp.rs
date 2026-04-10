use crate::agents::{self, AgentDef};
use crate::ar::ArEngine;
use crate::init;
use crate::thought::{ThinkingServer, ThoughtInput, Timestamp};
use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect},
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
    pub agents: HashMap<String, AgentDef>,
    pub ar: Option<ArEngine>,
    pub project_name: String,
    pub port: u16,
    pub oauth_codes: RwLock<HashMap<String, String>>,
}

pub struct Session {
    pub id: String,
    pub initialized: bool,
    pub created_at: Timestamp,
    pub last_activity: Timestamp,
    pub prefix: Option<String>,
    pub thinking_mode: Option<String>,
    pub artifact_type: Option<String>,
    pub ar_gated: bool,
    pub judge_cycle: u32,
    pub role: Option<String>,
    pub group: Option<String>,
}

impl McpState {
    pub fn new(server: ThinkingServer, agents: HashMap<String, AgentDef>, ar: Option<ArEngine>, project_name: String, port: u16) -> Self {
        Self {
            server,
            sessions: RwLock::new(HashMap::new()),
            agents,
            ar,
            project_name,
            port,
            oauth_codes: RwLock::new(HashMap::new()),
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
        .route("/.well-known/oauth-protected-resource", get(handle_oauth_protected_resource))
        .route("/.well-known/oauth-authorization-server", get(handle_oauth_metadata))
        .route("/oauth/authorize", get(handle_oauth_authorize))
        .route("/oauth/token", post(handle_oauth_token))
        .route("/oauth/register", post(handle_oauth_register))
        .route("/session/{id}", get(handle_session_lookup))
        .fallback(handle_fallback)
        .with_state(state)
}

async fn handle_fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({"error": "not_found"})),
    )
}

// --- OAuth endpoints (auto-approve for local dev) ---

fn oauth_base_url(headers: &HeaderMap, default_port: u16) -> String {
    if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        format!("http://{}", host)
    } else {
        format!("http://localhost:{}", default_port)
    }
}

async fn handle_oauth_protected_resource(headers: HeaderMap, State(state): State<Arc<McpState>>) -> Json<Value> {
    let base = oauth_base_url(&headers, state.port);
    Json(json!({
        "resource": base,
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"]
    }))
}

async fn handle_oauth_metadata(headers: HeaderMap, State(state): State<Arc<McpState>>) -> Json<Value> {
    let base = oauth_base_url(&headers, state.port);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{}/oauth/authorize", base),
        "token_endpoint": format!("{}/oauth/token", base),
        "registration_endpoint": format!("{}/oauth/register", base),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_post", "none"]
    }))
}

async fn handle_oauth_register(Json(body): Json<Value>) -> Json<Value> {
    let redirect_uris = body.get("redirect_uris").cloned().unwrap_or(json!([]));
    let client_name = body.get("client_name").and_then(|v| v.as_str()).unwrap_or("claude-code");
    Json(json!({
        "client_id": format!("feldspar-{}", uuid::Uuid::new_v4()),
        "client_secret": format!("secret-{}", uuid::Uuid::new_v4()),
        "client_id_issued_at": now_millis() / 1000,
        "client_secret_expires_at": 0,
        "redirect_uris": redirect_uris,
        "client_name": client_name,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "client_secret_post"
    }))
}

async fn handle_oauth_authorize(
    State(state): State<Arc<McpState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let redirect_uri = match params.get("redirect_uri") {
        Some(uri) => uri.clone(),
        None => return Redirect::temporary("/").into_response(),
    };
    let code = uuid::Uuid::new_v4().to_string();
    // Store the code for exchange
    state.oauth_codes.write().await.insert(code.clone(), "approved".to_string());
    let state_param = params.get("state").cloned().unwrap_or_default();
    let sep = if redirect_uri.contains('?') { "&" } else { "?" };
    let url = format!("{}{}code={}&state={}", redirect_uri, sep, code, state_param);
    Redirect::temporary(&url).into_response()
}

async fn handle_oauth_token(
    State(state): State<Arc<McpState>>,
    axum::Form(params): axum::Form<HashMap<String, String>>,
) -> Json<Value> {
    // Accept any code or refresh token
    if let Some(code) = params.get("code") {
        state.oauth_codes.write().await.remove(code);
    }
    Json(json!({
        "access_token": format!("feldspar-{}", uuid::Uuid::new_v4()),
        "token_type": "Bearer",
        "expires_in": 86400,
        "refresh_token": format!("refresh-{}", uuid::Uuid::new_v4())
    }))
}

pub async fn session_cleanup_task(state: Arc<McpState>) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    loop {
        interval.tick().await;
        let cutoff = now_millis() - SESSION_TTL_MS;

        let orphaned_prefixes: Vec<String> = {
            let mut sessions = state.sessions.write().await;
            let evicted_prefixes: Vec<String> = sessions.values()
                .filter(|s| s.last_activity <= cutoff)
                .filter_map(|s| s.prefix.clone())
                .collect();
            sessions.retain(|_, s| s.last_activity > cutoff);
            evicted_prefixes.into_iter()
                .filter(|p| !sessions.values().any(|s| s.prefix.as_deref() == Some(p.as_str())))
                .collect()
        };

        for prefix in orphaned_prefixes {
            let base = init::data_dir(&state.project_name);
            for mode in &["implementation", "debugging"] {
                let dir = base.join("artifacts/changes").join(mode).join(&prefix);
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

pub fn sweep_orphaned_changes(project_name: &str) {
    let base = init::data_dir(project_name);
    let threshold = std::time::Duration::from_secs(2 * 60 * 60);
    let now = std::time::SystemTime::now();
    for mode in &["implementation", "debugging"] {
        let changes_dir = base.join("artifacts/changes").join(mode);
        if let Ok(entries) = std::fs::read_dir(&changes_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let old_enough = entry.metadata().ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| now.duration_since(t).unwrap_or_default() > threshold)
                        .unwrap_or(false);
                    if old_enough {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }
    }
}

async fn handle_session_lookup(
    State(state): State<Arc<McpState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    match sessions.get(&session_id) {
        Some(s) => Json(json!({
            "prefix": s.prefix,
            "group": s.group,
            "role": s.role,
        })).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
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

    // Session validation (skip for initialize and tools/list)
    if method != "initialize" && method != "tools/list" {
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
        "tools/call" => DispatchResult::Response(handle_tools_call(state, headers, id, params).await, None),
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
            prefix: None,
            thinking_mode: None,
            artifact_type: None,
            ar_gated: false,
            judge_cycle: 0,
            role: None,
            group: None,
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

fn temper_tool_def() -> Value {
    json!({
        "name": "temper",
        "description": "Get role-specific agent instructions with active principles injected.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "role": {
                    "type": "string",
                    "enum": ["orchestrator", "arm", "solve", "breakdown", "build", "bugfest", "pmatch"],
                    "description": "Agent role to activate"
                },
                "prefix": {
                    "type": "string",
                    "description": "Reuse prefix from previous workflow step. If omitted, generates new."
                },
                "group": {
                    "type": "string",
                    "description": "Zero-padded group number for build agents (e.g., '01'). Omit for non-build roles."
                }
            },
            "required": ["role"]
        }
    })
}

fn sequentialthinking_tool_def() -> Value {
    json!({
        "name": "sequentialthinking",
        "description": concat!(
                    "Structured reasoning tool with cognitive analysis. Each call records one thought in a trace.\n\n",
                    "Always reason in English regardless of user language — the analyzers rely on English keyword detection.\n\n",
                    "Every thought is analyzed for cognitive biases, overconfidence, sycophancy, and reasoning depth. ",
                    "Warnings fire automatically when issues are detected. A recap is generated every few thoughts to prevent context drift. ",
                    "On completion, an ADR skeleton is generated.\n\n",
                    "Parameters:\n",
                    "- thought: Your current reasoning step.\n",
                    "- thoughtNumber: Current step (1-indexed).\n",
                    "- totalThoughts: Estimated total (adjust up or down as you progress).\n",
                    "- nextThoughtNeeded: True if more thinking needed. False to complete the trace.\n",
                    "- affectedComponents: System components involved in this decision.\n",
                    "- confidence: Your confidence in current reasoning (0-100). Independently calibrated.\n",
                    "- evidence: Citations -- file paths, docs, measurements, links. Earns confidence points.\n",
                    "- estimatedImpact: Expected impact -- latency, throughput, risk.\n",
                    "- isRevision: True if this revises a previous thought.\n",
                    "- revisesThought: Which thought number is being revised.\n",
                    "- branchFromThought: Fork point to explore an alternative approach.\n",
                    "- branchId: Label for the alternative branch.\n",
                    "- needsMoreThoughts: Signal that you need more thoughts beyond the original estimate.\n\n",
                    "Response includes: warnings, analyzer alerts, confidence calibration, budget status, ML trajectory score, pattern recall from similar past traces, and recap."
                ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "traceId": { "type": "string", "description": "Trace ID returned from thought 1. Required for thought 2+." },
                "thought": { "type": "string", "description": "Your current reasoning step." },
                "thoughtNumber": { "type": "integer", "description": "Current step (1-indexed)." },
                "totalThoughts": { "type": "integer", "description": "Estimated total." },
                "nextThoughtNeeded": { "type": "boolean", "description": "True if more thinking needed." },
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
    })
}

fn submit_tool_def() -> Value {
    json!({
        "name": "submit",
        "description": "REQUIRES `temper` first. Add a new unit to your assigned artifact. The exact field shape depends on your role (brief → requirement, design → module, execution_plan → task, diagnosis → diagnosis). See your temper response for required fields. Errors if name already exists.",
        "inputSchema": {
            "type": "object",
            "description": "Role-specific fields — see temper response for shape.",
            "additionalProperties": true
        }
    })
}

fn revise_tool_def() -> Value {
    json!({
        "name": "revise",
        "description": "REQUIRES `temper` first. Replace an existing unit in your artifact by name. Same shape as submit. Errors if name doesn't exist.",
        "inputSchema": {
            "type": "object",
            "description": "Role-specific fields — see temper response for shape.",
            "additionalProperties": true
        }
    })
}

fn remove_tool_def() -> Value {
    json!({
        "name": "remove",
        "description": "REQUIRES `temper` first. Remove a unit from your artifact by name (or number for validation_report).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Unit name (brief/design/execution_plan)" },
                "number": { "type": "integer", "description": "Claim number (validation_report only)" }
            },
            "additionalProperties": false
        }
    })
}

fn unit_schema_for(artifact_type: &str) -> (&str, Value) {
    match artifact_type {
        "brief" => ("requirement", crate::schemas::Requirement::json_schema()),
        "design" => ("module", crate::schemas::Module::json_schema()),
        "execution_plan" => ("task", crate::schemas::Task::json_schema()),
        "diagnosis" => ("diagnosis", crate::schemas::Diagnosis::json_schema()),
        "validation_report" => ("claim", crate::schemas::Claim::json_schema()),
        _ => unreachable!("unhandled artifact_type: {artifact_type}"),
    }
}

fn unit_key_for(artifact_type: &str) -> (&str, Value) {
    match artifact_type {
        "diagnosis" => ("diagnosis", json!({
            "type": "object", "properties": {}, "required": [],
            "additionalProperties": false
        })),
        "validation_report" => ("claim", json!({
            "type": "object",
            "properties": { "number": { "type": "integer", "description": "Claim number to remove" } },
            "required": ["number"], "additionalProperties": false
        })),
        "brief" => ("requirement", json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "Requirement name to remove" } },
            "required": ["name"], "additionalProperties": false
        })),
        "design" => ("module", json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "Module name to remove" } },
            "required": ["name"], "additionalProperties": false
        })),
        "execution_plan" => ("task", json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "Task name to remove" } },
            "required": ["name"], "additionalProperties": false
        })),
        _ => unreachable!("unhandled artifact_type: {artifact_type}"),
    }
}

fn deserialize_unit(artifact_type: &str, arguments: &serde_json::Value) -> Result<toml::Value, String> {
    match artifact_type {
        "brief" => {
            let unit: crate::schemas::Requirement = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid requirement: {e}"))?;
            toml::Value::try_from(unit).map_err(|e| format!("TOML conversion failed: {e}"))
        }
        "design" => {
            let unit: crate::schemas::Module = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid module: {e}"))?;
            toml::Value::try_from(unit).map_err(|e| format!("TOML conversion failed: {e}"))
        }
        "execution_plan" => {
            let unit: crate::schemas::Task = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid task: {e}"))?;
            toml::Value::try_from(unit).map_err(|e| format!("TOML conversion failed: {e}"))
        }
        "diagnosis" => {
            let unit: crate::schemas::Diagnosis = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid diagnosis: {e}"))?;
            toml::Value::try_from(unit).map_err(|e| format!("TOML conversion failed: {e}"))
        }
        "validation_report" => {
            let unit: crate::schemas::Claim = serde_json::from_value(arguments.clone())
                .map_err(|e| format!("Invalid claim: {e}"))?;
            toml::Value::try_from(unit).map_err(|e| format!("TOML conversion failed: {e}"))
        }
        _ => unreachable!("unhandled artifact_type: {artifact_type}"),
    }
}

fn judge_tool_def() -> Value {
    json!({
        "name": "judge",
        "description": "Evaluate a submitted artifact against coding principles and adversarial review.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Artifact name to evaluate" }
            },
            "required": ["name"]
        }
    })
}

fn configure_tool_def() -> Value {
    json!({
        "name": "configure",
        "description": "Manage feldspar principles and thinking modes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add_group", "add_principle", "activate", "deactivate", "add_mode", "remove_mode"],
                    "description": "Configuration action"
                },
                "level": { "type": "string", "enum": ["user", "project"], "description": "Config level" },
                "group": { "type": "string", "description": "Principle group name" },
                "active": { "type": "boolean", "description": "Group active state" },
                "name": { "type": "string", "description": "Principle or mode name" },
                "rule": { "type": "string", "description": "Principle rule" },
                "ask": { "type": "array", "items": {"type": "string"}, "description": "Check questions" },
                "budget": { "type": "string", "description": "Mode budget tier" },
                "requires": { "type": "array", "items": {"type": "string"}, "description": "Mode requirements" },
                "watches": { "type": "string", "description": "What mode watches" }
            },
            "required": ["action", "level"]
        }
    })
}

fn artifact_type_to_mode(artifact_type: &str) -> Option<&str> {
    match artifact_type {
        "brief" => Some("brainstorming"),
        "design" => Some("problem-solving"),
        "execution_plan" => Some("planning"),
        "diagnosis" => Some("debugging"),
        "validation_report" => Some("pattern-matching"),
        _ => None,
    }
}

fn toml_key_for(artifact_type: &str) -> (&str, Option<&str>) {
    match artifact_type {
        "brief" => ("requirements", Some("name")),
        "design" => ("modules", Some("name")),
        "execution_plan" => ("tasks", Some("name")),
        "diagnosis" => ("diagnosis", None),
        "validation_report" => ("claims", Some("number")),
        _ => unreachable!("unhandled artifact_type: {artifact_type}"),
    }
}

fn is_singleton(artifact_type: &str) -> bool {
    artifact_type == "diagnosis"
}

fn artifact_path(state: &McpState, prefix: &str, mode: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home)
        .join(".feldspar/data")
        .join(&state.project_name)
        .join("artifacts")
        .join(mode)
        .join(format!("{}.toml", prefix))
}

fn extract_key(arguments: &serde_json::Value, key_field: Option<&str>) -> Result<toml::Value, String> {
    let field = key_field.ok_or_else(|| "No key field for singleton type".to_owned())?;
    let val = arguments.get(field).ok_or_else(|| format!("Missing key field: {field}"))?;
    match val {
        serde_json::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_json::Value::Number(n) => {
            let i = n.as_i64().ok_or_else(|| "Key number must be integer".to_owned())?;
            Ok(toml::Value::Integer(i))
        }
        _ => Err(format!("Unsupported key type for field: {field}")),
    }
}

fn find_unit_index(arr: &[toml::Value], key_field: &str, key_value: &toml::Value) -> Option<usize> {
    arr.iter().position(|entry| {
        entry.as_table()
            .and_then(|t| t.get(key_field))
            .map(|v| v == key_value)
            .unwrap_or(false)
    })
}

fn fetch_tool_def() -> Value {
    json!({
        "name": "fetch",
        "description": "Read a previously submitted artifact by prefix and type.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prefix": { "type": "string", "description": "Workflow prefix" },
                "type": {
                    "type": "string",
                    "enum": ["brief", "design", "execution_plan", "diagnosis", "validation_report"],
                    "description": "Artifact type to fetch"
                }
            },
            "required": ["prefix", "type"]
        }
    })
}

async fn handle_fetch(state: &McpState, headers: &HeaderMap, id: Value, params: Option<Value>) -> Value {
    let session_id = match validate_session(state, headers).await {
        Ok(id) => id,
        Err(_) => return jsonrpc_error(Some(id), -32602, "No valid session"),
    };
    let sessions = state.sessions.read().await;
    if !sessions.get(&session_id).map(|s| s.prefix.is_some()).unwrap_or(false) {
        return jsonrpc_error(Some(id), -32602, "Must call temper first");
    }
    drop(sessions);

    let arguments = params.as_ref().and_then(|p| p.get("arguments")).cloned().unwrap_or_default();
    let prefix = arguments.get("prefix").and_then(|p| p.as_str()).unwrap_or("");
    let artifact_type = arguments.get("type").and_then(|t| t.as_str()).unwrap_or("");

    if prefix.is_empty() || artifact_type.is_empty() {
        return jsonrpc_error(Some(id), -32602, "Missing prefix or type");
    }

    let mode = match artifact_type_to_mode(artifact_type) {
        Some(m) => m,
        None => return jsonrpc_error(Some(id), -32602, &format!("Unknown artifact type: {}", artifact_type)),
    };

    let path = artifact_path(state, prefix, mode);
    let content = std::fs::read_to_string(&path).ok();

    match content {
        Some(c) => jsonrpc_result(id, json!({
            "content": [{"type": "text", "text": c}],
            "isError": false
        })),
        None => jsonrpc_error(Some(id), -32602,
            &format!("Artifact not found: prefix={}, type={}", prefix, artifact_type)),
    }
}

fn resolve_config_dir(project_name: &str, level: &str) -> std::path::PathBuf {
    match level {
        "user" => crate::init::user_config_dir(),
        _ => crate::init::data_dir(project_name).join("config"),
    }
}

fn ok_response(id: Value, message: &str) -> Value {
    jsonrpc_result(id, json!({
        "content": [{"type": "text", "text": message}],
        "isError": false
    }))
}

fn handle_configure(state: &McpState, id: Value, params: &Value) -> Value {
    let arguments = match params.get("arguments") {
        Some(args) => args.clone(),
        None => return jsonrpc_error(Some(id), -32602, "Missing arguments"),
    };

    let action = match arguments.get("action").and_then(|a| a.as_str()) {
        Some(a) => a.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: action"),
    };

    let level = match arguments.get("level").and_then(|l| l.as_str()) {
        Some(l) => l.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: level"),
    };

    let config_dir = resolve_config_dir(&state.project_name, &level);

    match action.as_str() {
        "list" => configure_list(id, &config_dir, &level),
        "add_group" => configure_add_group(id, &arguments, &config_dir),
        "add_principle" => configure_add_principle(id, &arguments, &config_dir),
        "activate" => configure_set_active(id, &arguments, &config_dir, true),
        "deactivate" => configure_set_active(id, &arguments, &config_dir, false),
        "add_mode" => configure_add_mode(id, &arguments, &config_dir),
        "remove_mode" => configure_remove_mode(id, &arguments, &config_dir),
        _ => jsonrpc_error(Some(id), -32602, &format!("Unknown action: {}", action)),
    }
}

fn configure_list(id: Value, config_dir: &std::path::Path, level: &str) -> Value {
    let principles_content = std::fs::read_to_string(config_dir.join("principles.toml")).unwrap_or_default();
    let principles_val: toml::Value = toml::from_str(&principles_content)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));

    let groups_list: Vec<Value> = principles_val
        .get("groups")
        .and_then(|g| g.as_table())
        .map(|groups| {
            groups.iter().map(|(name, group)| {
                let active = group.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
                let count = group.get("principles")
                    .and_then(|p| p.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                json!({ "group": name, "active": active, "count": count })
            }).collect()
        })
        .unwrap_or_default();

    let feldspar_content = std::fs::read_to_string(config_dir.join("feldspar.toml")).unwrap_or_default();
    let feldspar_val: toml::Value = toml::from_str(&feldspar_content)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));

    let modes_list: Vec<Value> = feldspar_val
        .get("modes")
        .and_then(|m| m.as_table())
        .map(|modes| {
            modes.iter().map(|(name, mode)| {
                let budget = mode.get("budget").and_then(|b| b.as_str()).unwrap_or("unknown");
                json!({ "name": name, "budget": budget })
            }).collect()
        })
        .unwrap_or_default();

    ok_response(id, &serde_json::to_string(&json!({
        "principles": groups_list,
        "modes": modes_list,
        "level": level
    })).unwrap_or_default())
}

fn configure_add_group(id: Value, arguments: &Value, config_dir: &std::path::Path) -> Value {
    let group = match arguments.get("group").and_then(|g| g.as_str()) {
        Some(g) => g.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: group"),
    };
    let active = arguments.get("active").and_then(|a| a.as_bool()).unwrap_or(true);

    let path = config_dir.join("principles.toml");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();

    if content.contains(&format!("[groups.{}]", group)) {
        return jsonrpc_error(Some(id), -32602, &format!("Group '{}' already exists", group));
    }

    content.push_str(&format!("\n[groups.{}]\nactive = {}\nprinciples = []\n", group, active));

    std::fs::create_dir_all(config_dir).ok();
    match std::fs::write(&path, &content) {
        Ok(_) => ok_response(id, &format!("Group '{}' added (active: {})", group, active)),
        Err(e) => jsonrpc_error(Some(id), -32603, &format!("Failed to write: {}", e)),
    }
}

fn configure_add_principle(id: Value, arguments: &Value, config_dir: &std::path::Path) -> Value {
    let group = match arguments.get("group").and_then(|g| g.as_str()) {
        Some(g) => g.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: group"),
    };
    let name = match arguments.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: name"),
    };
    let rule = match arguments.get("rule").and_then(|r| r.as_str()) {
        Some(r) => r.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: rule"),
    };
    let ask: Vec<String> = arguments.get("ask")
        .and_then(|a| serde_json::from_value(a.clone()).ok())
        .unwrap_or_default();

    let path = config_dir.join("principles.toml");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();

    if !content.contains(&format!("[groups.{}]", group)) {
        return jsonrpc_error(Some(id), -32602, &format!("Group '{}' not found. Use add_group first.", group));
    }

    let ask_str = ask.iter()
        .map(|a| format!("  \"{}\",", a))
        .collect::<Vec<_>>()
        .join("\n");
    content.push_str(&format!(
        "\n[[groups.{}.principles]]\nname = \"{}\"\nrule = \"{}\"\nask = [\n{}\n]\n",
        group, name, rule, ask_str
    ));

    match std::fs::write(&path, &content) {
        Ok(_) => ok_response(id, &format!("Principle '{}' added to group '{}'", name, group)),
        Err(e) => jsonrpc_error(Some(id), -32603, &format!("Failed to write: {}", e)),
    }
}

fn configure_set_active(id: Value, arguments: &Value, config_dir: &std::path::Path, active: bool) -> Value {
    let group = match arguments.get("group").and_then(|g| g.as_str()) {
        Some(g) => g.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: group"),
    };

    let path = config_dir.join("principles.toml");
    let content = std::fs::read_to_string(&path).unwrap_or_default();

    let mut val: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => toml::Value::Table(Default::default()),
    };

    let group_exists = val.get("groups")
        .and_then(|g| g.as_table())
        .map(|t| t.contains_key(&group))
        .unwrap_or(false);

    if !group_exists {
        return jsonrpc_error(Some(id), -32602, &format!("Group '{}' not found", group));
    }

    if let Some(groups) = val.get_mut("groups").and_then(|g| g.as_table_mut()) {
        if let Some(g) = groups.get_mut(&group).and_then(|g| g.as_table_mut()) {
            g.insert("active".to_owned(), toml::Value::Boolean(active));
        }
    }

    let serialized = toml::to_string_pretty(&val).unwrap_or_default();
    match std::fs::write(&path, &serialized) {
        Ok(_) => ok_response(id, &format!("Group '{}' set active={}", group, active)),
        Err(e) => jsonrpc_error(Some(id), -32603, &format!("Failed to write: {}", e)),
    }
}

fn configure_add_mode(id: Value, arguments: &Value, config_dir: &std::path::Path) -> Value {
    let name = match arguments.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: name"),
    };

    if name.contains(' ') || name.chars().any(|c| c.is_uppercase()) {
        return jsonrpc_error(Some(id), -32602, "Mode name must be lowercase with no spaces");
    }

    let budget = arguments.get("budget").and_then(|b| b.as_str()).unwrap_or("standard");
    let requires: Vec<String> = arguments.get("requires")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
        .unwrap_or_default();
    let watches = arguments.get("watches").and_then(|w| w.as_str()).unwrap_or("");

    let toml_path = config_dir.join("feldspar.toml");
    let mut content = std::fs::read_to_string(&toml_path).unwrap_or_default();

    if content.contains(&format!("[modes.{}]", name)) {
        return jsonrpc_error(Some(id), -32602, &format!("Mode '{}' already exists", name));
    }

    let requires_str = requires.iter().map(|r| format!("\"{}\"", r)).collect::<Vec<_>>().join(", ");
    content.push_str(&format!(
        "\n[modes.{}]\nrequires = [{}]\nbudget = \"{}\"\nwatches = \"{}\"\n",
        name, requires_str, budget, watches
    ));
    std::fs::create_dir_all(config_dir).ok();
    std::fs::write(&toml_path, &content).ok();

    let agents_dir = config_dir.join("agents");
    std::fs::create_dir_all(&agents_dir).ok();
    let agent_toml = format!(r#"[agent]
name = "{name}"
artifact_type = "code"
interactive = "background"
team = true
ar_gated = true
thinking_mode = "{name}"

[prompt]
identity = """
You are a {name} agent.
"""

instructions = """
Follow the active principles. Cite evidence. Challenge your reasoning.
"""

[warnings]
mode = []

[shutdown]
instruction = """
When you receive a shutdown_request message, use the SendMessage tool
to reply to the team lead with this exact JSON as the message parameter:
{{"type": "shutdown_response", "request_id": "[request_id]", "approve": true}}
Do NOT print this as text. You MUST use the SendMessage tool. Then stop all work.
"""
"#);
    std::fs::write(agents_dir.join(format!("{}.toml", name)), &agent_toml).ok();

    ok_response(id, &format!("Mode '{}' added (budget: {}). Agent auto-created.", name, budget))
}

fn configure_remove_mode(id: Value, arguments: &Value, config_dir: &std::path::Path) -> Value {
    let name = match arguments.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_owned(),
        None => return jsonrpc_error(Some(id), -32602, "Missing required argument: name"),
    };

    let toml_path = config_dir.join("feldspar.toml");
    let content = std::fs::read_to_string(&toml_path).unwrap_or_default();

    let mut val: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => toml::Value::Table(Default::default()),
    };

    let removed = val.get_mut("modes")
        .and_then(|m| m.as_table_mut())
        .map(|modes| modes.remove(&name).is_some())
        .unwrap_or(false);

    if !removed {
        return jsonrpc_error(Some(id), -32602, &format!("Mode '{}' not found", name));
    }

    let serialized = toml::to_string_pretty(&val).unwrap_or_default();
    std::fs::write(&toml_path, &serialized).ok();

    std::fs::remove_file(config_dir.join("agents").join(format!("{}.toml", name))).ok();

    ok_response(id, &format!("Mode '{}' removed", name))
}

fn handle_tools_list(id: Value) -> Value {
    let tools = vec![
        temper_tool_def(),
        configure_tool_def(),
        sequentialthinking_tool_def(),
        submit_tool_def(),
        revise_tool_def(),
        remove_tool_def(),
        fetch_tool_def(),
        judge_tool_def(),
    ];
    jsonrpc_result(id, json!({ "tools": tools }))
}

async fn handle_tools_call(state: &McpState, headers: &HeaderMap, id: Value, params: Option<Value>) -> Value {
    let params = match params {
        Some(p) => p,
        None => return jsonrpc_error(Some(id), -32602, "Missing params"),
    };

    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");

    match name {
        "temper" => {
            let arguments = match params.get("arguments") {
                Some(args) => args,
                None => return jsonrpc_error(Some(id), -32602, "Missing arguments"),
            };
            let role = arguments.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role.is_empty() {
                return jsonrpc_error(Some(id), -32602, "Missing required argument: role");
            }

            // Validate group for build role before anything else
            let group_opt: Option<String> = if role == "build" {
                let group = arguments.get("group").and_then(|g| g.as_str()).unwrap_or("");
                if group.len() == 2 && group.chars().all(|c| c.is_ascii_digit()) {
                    Some(group.to_owned())
                } else {
                    return jsonrpc_error(Some(id), -32602,
                        "group must be a two-digit zero-padded number (e.g., '01')");
                }
            } else {
                None
            };

            match state.agents.get(role) {
                Some(agent) => {
                    let is_orchestrator = role == "orchestrator";
                    let prefix = if is_orchestrator {
                        String::new()
                    } else {
                        match arguments.get("prefix").and_then(|p| p.as_str()) {
                            Some(p) if !p.is_empty() => p.to_owned(),
                            _ => loop {
                                let candidate = agents::generate_prefix();
                                let sessions = state.sessions.read().await;
                                let taken = sessions.values().any(|s| s.prefix.as_deref() == Some(&candidate));
                                drop(sessions);
                                if !taken { break candidate; }
                            },
                        }
                    };
                    let prompt = agents::temper(agent, &state.server.config, &prefix);

                    if let Ok(session_id) = validate_session(state, headers).await {
                        let mut sessions = state.sessions.write().await;
                        if let Some(session) = sessions.get_mut(&session_id) {
                            if !is_orchestrator {
                                session.prefix = Some(prefix.clone());
                            }
                            session.thinking_mode = Some(agent.thinking_mode.clone());
                            session.artifact_type = Some(agent.artifact_type.clone());
                            session.ar_gated = agent.ar_gated;
                            session.judge_cycle = 0;
                            session.role = Some(role.to_owned());
                            session.group = group_opt.clone();
                        }
                    }

                    // Create empty change file for build/bugfest (best-effort)
                    if role == "build" || role == "bugfest" {
                        let mode = if role == "build" { "implementation" } else { "debugging" };
                        let changes_dir = init::data_dir(&state.project_name)
                            .join("artifacts/changes").join(mode).join(&prefix);
                        let _ = std::fs::create_dir_all(&changes_dir);
                        let filename = if role == "build" {
                            format!("{}-changes.toml", group_opt.as_deref().unwrap_or("00"))
                        } else {
                            "changes.toml".to_owned()
                        };
                        let _ = std::fs::File::create(changes_dir.join(filename));
                    }

                    jsonrpc_result(
                        id,
                        json!({
                            "content": [{"type": "text", "text": prompt}],
                            "isError": false
                        }),
                    )
                }
                None => jsonrpc_error(Some(id), -32602, &format!("Unknown agent role: {}", role)),
            }
        }
        "configure" => handle_configure(state, id, &params),
        "submit" => handle_submit(state, headers, id, Some(params)).await,
        "revise" => handle_revise(state, headers, id, Some(params)).await,
        "remove" => handle_remove(state, headers, id, Some(params)).await,
        "fetch" => handle_fetch(state, headers, id, Some(params)).await,
        "judge" => handle_judge(state, headers, id, Some(params)).await,
        "sequentialthinking" => {
            let arguments = match params.get("arguments") {
                Some(args) => args,
                None => return jsonrpc_error(Some(id), -32602, "Missing arguments"),
            };

            let mut input: ThoughtInput = match serde_json::from_value(arguments.clone()) {
                Ok(i) => i,
                Err(e) => return jsonrpc_error(Some(id), -32602, &format!("Invalid arguments: {}", e)),
            };

            // Inject thinking_mode from session (set by temper)
            if let Ok(sid) = validate_session(state, headers).await {
                let sessions = state.sessions.read().await;
                if let Some(session) = sessions.get(&sid) {
                    input.thinking_mode = session.thinking_mode.clone();
                }
            }

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
        _ => jsonrpc_error(Some(id), -32602, &format!("Unknown tool: {}", name)),
    }
}

async fn handle_submit(state: &McpState, headers: &HeaderMap, id: Value, params: Option<Value>) -> Value {
    let session_id = match validate_session(state, headers).await {
        Ok(id) => id,
        Err(_) => return jsonrpc_error(Some(id), -32602, "No valid session"),
    };

    let (prefix, mode, artifact_type) = {
        let sessions = state.sessions.read().await;
        let session = match sessions.get(&session_id) {
            Some(s) => s,
            None => return jsonrpc_error(Some(id), -32602, "Session not found"),
        };
        let prefix = match &session.prefix {
            Some(p) => p.clone(),
            None => return jsonrpc_error(Some(id), -32602, "Must call temper first"),
        };
        let mode = session.thinking_mode.clone().unwrap_or_else(|| "unknown".into());
        let artifact_type = match &session.artifact_type {
            Some(at) => at.clone(),
            None => return jsonrpc_error(Some(id), -32602, "No artifact type set"),
        };
        (prefix, mode, artifact_type)
    };

    let arguments = params.as_ref()
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_default();

    let (toml_key, key_field) = toml_key_for(&artifact_type);
    let path = artifact_path(state, &prefix, &mode);

    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to create artifact directory");
        }
    }

    if is_singleton(&artifact_type) {
        if path.exists() {
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            if !existing.trim().is_empty() {
                return jsonrpc_error(Some(id), -32602, "Diagnosis already exists. Use revise to update.");
            }
        }
        let unit_value = match deserialize_unit(&artifact_type, &arguments) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &e),
        };
        let unit_toml = match toml::to_string(&unit_value) {
            Ok(s) => s,
            Err(e) => return jsonrpc_error(Some(id), -32603, &format!("TOML serialization failed: {e}")),
        };
        let content = format!("[diagnosis]\n{}", unit_toml);
        if std::fs::write(&path, content).is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to write artifact");
        }
    } else {
        let key_value = match extract_key(&arguments, key_field) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &e),
        };

        if path.exists() {
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            if !existing.trim().is_empty() {
                if let Ok(doc) = toml::from_str::<toml::Value>(&existing) {
                    if let Some(arr) = doc.as_table().and_then(|t| t.get(toml_key)).and_then(|v| v.as_array()) {
                        if find_unit_index(arr, key_field.unwrap(), &key_value).is_some() {
                            return jsonrpc_error(Some(id), -32602,
                                &format!("{} already exists. Use revise to update.", toml_key));
                        }
                    }
                }
            }
        }

        let unit_value = match deserialize_unit(&artifact_type, &arguments) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &e),
        };
        let unit_toml = match toml::to_string(&unit_value) {
            Ok(s) => s,
            Err(e) => return jsonrpc_error(Some(id), -32603, &format!("TOML serialization failed: {e}")),
        };

        let block = format!("\n[[{}]]\n{}", toml_key, unit_toml);
        use std::io::Write;
        let mut file = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(_) => return jsonrpc_error(Some(id), -32603, "Failed to open artifact file"),
        };
        if file.write_all(block.as_bytes()).is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to write artifact");
        }
    }

    jsonrpc_result(id, json!({
        "content": [{"type": "text", "text": "ok"}],
        "isError": false
    }))
}

async fn handle_revise(state: &McpState, headers: &HeaderMap, id: Value, params: Option<Value>) -> Value {
    let session_id = match validate_session(state, headers).await {
        Ok(id) => id,
        Err(_) => return jsonrpc_error(Some(id), -32602, "No valid session"),
    };

    let (prefix, mode, artifact_type) = {
        let sessions = state.sessions.read().await;
        let session = match sessions.get(&session_id) {
            Some(s) => s,
            None => return jsonrpc_error(Some(id), -32602, "Session not found"),
        };
        let prefix = match &session.prefix {
            Some(p) => p.clone(),
            None => return jsonrpc_error(Some(id), -32602, "Must call temper first"),
        };
        let mode = session.thinking_mode.clone().unwrap_or_else(|| "unknown".into());
        let artifact_type = match &session.artifact_type {
            Some(at) => at.clone(),
            None => return jsonrpc_error(Some(id), -32602, "No artifact type set"),
        };
        (prefix, mode, artifact_type)
    };

    let arguments = params.as_ref()
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_default();

    let (toml_key, key_field) = toml_key_for(&artifact_type);
    let path = artifact_path(state, &prefix, &mode);

    if !path.exists() {
        return jsonrpc_error(Some(id), -32602, "No artifact to revise");
    }

    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return jsonrpc_error(Some(id), -32603, "Failed to read artifact"),
    };

    let unit_value = match deserialize_unit(&artifact_type, &arguments) {
        Ok(v) => v,
        Err(e) => return jsonrpc_error(Some(id), -32602, &e),
    };

    if is_singleton(&artifact_type) {
        let mut doc: toml::Value = match toml::from_str(&existing) {
            Ok(v) => v,
            Err(_) => toml::Value::Table(toml::map::Map::new()),
        };
        if let Some(table) = doc.as_table_mut() {
            if let toml::Value::Table(unit_table) = unit_value {
                table.insert("diagnosis".to_owned(), toml::Value::Table(unit_table));
            }
        }
        let toml_str = match toml::to_string(&doc) {
            Ok(s) => s,
            Err(e) => return jsonrpc_error(Some(id), -32603, &format!("TOML serialization failed: {e}")),
        };
        if let Err(e) = crate::schemas::validate(&artifact_type, &toml_str) {
            return jsonrpc_error(Some(id), -32602, &format!("Revise produced invalid artifact: {e}"));
        }
        if std::fs::write(&path, toml_str).is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to write artifact");
        }
    } else {
        let key_value = match extract_key(&arguments, key_field) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &e),
        };

        let mut doc: toml::Value = match toml::from_str(&existing) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &format!("Failed to parse artifact: {e}")),
        };

        let arr = match doc.as_table_mut()
            .and_then(|t| t.get_mut(toml_key))
            .and_then(|v| v.as_array_mut())
        {
            Some(a) => a,
            None => return jsonrpc_error(Some(id), -32602,
                &format!("{} '{}' not found. Use submit to create.", toml_key,
                    match &key_value { toml::Value::String(s) => s.clone(), toml::Value::Integer(n) => n.to_string(), _ => "?".into() })),
        };

        let idx = match find_unit_index(arr, key_field.unwrap(), &key_value) {
            Some(i) => i,
            None => return jsonrpc_error(Some(id), -32602,
                &format!("{} '{}' not found. Use submit to create.", toml_key,
                    match &key_value { toml::Value::String(s) => s.clone(), toml::Value::Integer(n) => n.to_string(), _ => "?".into() })),
        };

        arr[idx] = unit_value;

        let toml_str = match toml::to_string(&doc) {
            Ok(s) => s,
            Err(e) => return jsonrpc_error(Some(id), -32603, &format!("TOML serialization failed: {e}")),
        };
        if let Err(e) = crate::schemas::validate(&artifact_type, &toml_str) {
            return jsonrpc_error(Some(id), -32602, &format!("Revise produced invalid artifact: {e}"));
        }
        if std::fs::write(&path, toml_str).is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to write artifact");
        }
    }

    jsonrpc_result(id, json!({
        "content": [{"type": "text", "text": "ok"}],
        "isError": false
    }))
}

async fn handle_remove(state: &McpState, headers: &HeaderMap, id: Value, params: Option<Value>) -> Value {
    let session_id = match validate_session(state, headers).await {
        Ok(id) => id,
        Err(_) => return jsonrpc_error(Some(id), -32602, "No valid session"),
    };

    let (prefix, mode, artifact_type) = {
        let sessions = state.sessions.read().await;
        let session = match sessions.get(&session_id) {
            Some(s) => s,
            None => return jsonrpc_error(Some(id), -32602, "Session not found"),
        };
        let prefix = match &session.prefix {
            Some(p) => p.clone(),
            None => return jsonrpc_error(Some(id), -32602, "Must call temper first"),
        };
        let mode = session.thinking_mode.clone().unwrap_or_else(|| "unknown".into());
        let artifact_type = match &session.artifact_type {
            Some(at) => at.clone(),
            None => return jsonrpc_error(Some(id), -32602, "No artifact type set"),
        };
        (prefix, mode, artifact_type)
    };

    let arguments = params.as_ref()
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_default();

    let (toml_key, key_field) = toml_key_for(&artifact_type);
    let path = artifact_path(state, &prefix, &mode);

    if !path.exists() {
        return jsonrpc_error(Some(id), -32602, "No artifact to remove from");
    }

    if is_singleton(&artifact_type) {
        if std::fs::write(&path, "").is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to truncate artifact");
        }
    } else {
        let existing = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return jsonrpc_error(Some(id), -32603, "Failed to read artifact"),
        };

        let key_value = match extract_key(&arguments, key_field) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &e),
        };

        let mut doc: toml::Value = match toml::from_str(&existing) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(Some(id), -32602, &format!("Failed to parse artifact: {e}")),
        };

        let arr = match doc.as_table_mut()
            .and_then(|t| t.get_mut(toml_key))
            .and_then(|v| v.as_array_mut())
        {
            Some(a) => a,
            None => return jsonrpc_error(Some(id), -32602,
                &format!("{} '{}' not found.", toml_key,
                    match &key_value { toml::Value::String(s) => s.clone(), toml::Value::Integer(n) => n.to_string(), _ => "?".into() })),
        };

        let idx = match find_unit_index(arr, key_field.unwrap(), &key_value) {
            Some(i) => i,
            None => return jsonrpc_error(Some(id), -32602,
                &format!("{} '{}' not found.", toml_key,
                    match &key_value { toml::Value::String(s) => s.clone(), toml::Value::Integer(n) => n.to_string(), _ => "?".into() })),
        };

        arr.remove(idx);

        let toml_str = match toml::to_string(&doc) {
            Ok(s) => s,
            Err(e) => return jsonrpc_error(Some(id), -32603, &format!("TOML serialization failed: {e}")),
        };
        if std::fs::write(&path, toml_str).is_err() {
            return jsonrpc_error(Some(id), -32603, "Failed to write artifact");
        }
    }

    jsonrpc_result(id, json!({
        "content": [{"type": "text", "text": "ok"}],
        "isError": false
    }))
}

async fn handle_judge(state: &McpState, headers: &HeaderMap, id: Value, params: Option<Value>) -> Value {
    let session_id = match validate_session(state, headers).await {
        Ok(id) => id,
        Err(_) => return jsonrpc_error(Some(id), -32602, "No valid session"),
    };

    let (prefix, mode, cycle) = {
        let sessions = state.sessions.read().await;
        let session = match sessions.get(&session_id) {
            Some(s) => s,
            None => return jsonrpc_error(Some(id), -32602, "Session not found"),
        };
        let prefix = match &session.prefix {
            Some(p) => p.clone(),
            None => return jsonrpc_error(Some(id), -32602, "Must call temper first"),
        };
        (prefix, session.thinking_mode.clone().unwrap_or_default(), session.judge_cycle)
    };

    let ar_engine = match &state.ar {
        Some(e) => e,
        None => {
            let result = json!({
                "score": 0,
                "verdict": "approve",
                "cycle": cycle + 1,
                "maxCycles": 0,
                "feedback": {"note": "AR unavailable — auto-approved"}
            });
            return jsonrpc_result(id, json!({
                "content": [{"type": "text", "text": result.to_string()}],
                "isError": false
            }));
        }
    };

    let arguments = params.as_ref().and_then(|p| p.get("arguments")).cloned().unwrap_or_default();
    let name = arguments.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if name.is_empty() {
        return jsonrpc_error(Some(id), -32602, "Missing artifact name");
    }

    let artifact_name = format!("{}-{}", prefix, name);
    let project = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "default".into());

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let base_path = std::path::PathBuf::from(home)
        .join(".feldspar/data")
        .join(&project)
        .join("artifacts")
        .join(&mode);

    let path = base_path.join(format!("{}.toml", artifact_name));

    let artifact = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return jsonrpc_error(Some(id), -32602, &format!("Artifact '{}' not found", artifact_name)),
    };

    let full_artifact = {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&session_id);
        let role = session.and_then(|s| s.role.as_deref()).unwrap_or("");
        let group = session.and_then(|s| s.group.as_deref());
        let prefix_val = session.and_then(|s| s.prefix.as_deref()).unwrap_or("");

        if (role == "build" || role == "bugfest") && !prefix_val.is_empty() {
            let mode_dir = if role == "build" { "implementation" } else { "debugging" };
            let filename = if role == "build" {
                format!("{}-changes.toml", group.unwrap_or("00"))
            } else {
                "changes.toml".to_owned()
            };
            let changes_path = init::data_dir(&project)
                .join("artifacts/changes")
                .join(mode_dir)
                .join(prefix_val)
                .join(filename);
            if let Ok(changes) = std::fs::read_to_string(&changes_path) {
                format!("{}\n\n## Code Changes\n{}", artifact, changes)
            } else {
                artifact
            }
        } else {
            artifact
        }
    };

    let result = ar_engine
        .evaluate(&full_artifact, &state.server.config.principles, cycle)
        .await;

    if let Some(ref db) = state.server.db {
        let db = db.clone();
        let trace_id = session_id.clone();
        let mode_c = mode.clone();
        let p = result.principles_score;
        let a = result.adversarial_score;
        let c = result.combined_score;
        let v = result.verdict.as_str().to_owned();
        let cy = cycle + 1;
        let fb = format!("p: {}; a: {}",
            result.feedback.principles.join("; "),
            result.feedback.adversarial.join("; "));
        tokio::spawn(async move {
            db.store_ar_score(&trace_id, &mode_c, "artifact", p, a, c, &v, cy, &fb).await;
        });
    }

    {
        let mut sessions = state.sessions.write().await;
        if let Some(session) = sessions.get_mut(&session_id) {
            session.judge_cycle += 1;
        }
    }

    let verdict_json = json!({
        "score": result.combined_score,
        "verdict": result.verdict.as_str(),
        "cycle": cycle + 1,
        "maxCycles": ar_engine.max_retries,
        "feedback": {
            "principles": result.feedback.principles,
            "adversarial": result.feedback.adversarial,
        }
    });

    jsonrpc_result(id, json!({
        "content": [{"type": "text", "text": verdict_json.to_string()}],
        "isError": false
    }))
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
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
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
            ar: None,
            principles: vec![],
        })
    }

    #[test]
    fn test_sequentialthinking_schema_no_thinking_mode() {
        let tool = sequentialthinking_tool_def();
        assert!(tool["inputSchema"]["properties"]["thinkingMode"].is_null());
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
            None,
        );
        let agent_defs = crate::agents::load_agents("test");
        let state = Arc::new(McpState::new(server, agent_defs, None, "test".into(), 0));
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

    #[test]
    fn test_tools_list_returns_all_tools() {
        // tools/list always returns the full flat list. Per-role tool selection
        // is communicated via the temper response prompt, not via schema gating.
        let result = handle_tools_list(json!(1));
        let tools = result["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in &["temper", "configure", "sequentialthinking", "submit", "revise", "remove", "fetch", "judge"] {
            assert!(names.contains(expected), "missing tool: {}", expected);
        }
        assert_eq!(names.len(), 8);
    }

    #[tokio::test]
    async fn test_temper_valid_role() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        assert!(!body["result"]["isError"].as_bool().unwrap_or(true));
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.is_empty());
        assert!(text.contains("build agent"));
    }

    #[tokio::test]
    async fn test_temper_unknown_role() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"nonexistent"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"].as_str().unwrap().contains("Unknown agent role"));
    }

    #[tokio::test]
    async fn test_temper_missing_role() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_tools_list_has_sequentialthinking() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
            &session_id,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let tools = body["result"]["tools"].as_array().unwrap();
        let st = tools.iter().find(|t| t["name"] == "sequentialthinking").unwrap();
        assert!(st["inputSchema"]["properties"]["traceId"].is_object());
    }

    #[tokio::test]
    async fn test_tools_call_valid_thought() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
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
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
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
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
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
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{}}}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_request_with_invalid_session_returns_404() {
        let app = test_app();
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
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

        // 3. temper to establish role
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;

        // 4. tools/call thought 1 (no traceId)
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"first thought","thoughtNumber":1,"totalThoughts":2,"nextThoughtNeeded":true}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let wire: Value = serde_json::from_str(text).unwrap();
        let trace_id = wire["traceId"].as_str().unwrap().to_owned();
        assert_eq!(wire["thoughtNumber"], 1);

        // 5. tools/call thought 2 (with traceId)
        let call2 = format!(
            r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"sequentialthinking","arguments":{{"traceId":"{trace_id}","thought":"second thought","thoughtNumber":2,"totalThoughts":2,"nextThoughtNeeded":false}}}}}}"#
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
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"check","thoughtNumber":1,"totalThoughts":1,"nextThoughtNeeded":false}}}"#,
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

    // --- AR tools tests ---

    #[tokio::test]
    async fn test_temper_sets_prefix_in_session() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none());
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("PREFIX: "));
    }

    #[tokio::test]
    async fn test_tools_list_always_full_flat_set() {
        // Client discovery is one-shot (Claude Code only calls tools/list once).
        // The server returns the full flat tool set regardless of session state.
        // Per-role tool applicability is communicated via the temper response.
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let tools = body["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(tools.len(), 8, "expected 8 tools, got: {:?}", names);
        for expected in &["temper", "configure", "sequentialthinking", "submit", "revise", "remove", "fetch", "judge"] {
            assert!(names.contains(expected), "missing tool: {}", expected);
        }
    }

    #[tokio::test]
    async fn test_sequentialthinking_uses_session_mode() {
        // After temper(build), sequentialthinking uses session's thinking_mode (no thinkingMode in args)
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"test","thoughtNumber":1,"totalThoughts":1,"nextThoughtNeeded":false}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        assert!(!body["result"]["isError"].as_bool().unwrap_or(true));
    }

    #[tokio::test]
    async fn test_sequentialthinking_ignores_client_mode() {
        // After temper(build), thinkingMode in args is deserialized but overwritten by session mode
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sequentialthinking","arguments":{"thought":"test","thoughtNumber":1,"totalThoughts":1,"nextThoughtNeeded":false,"thinkingMode":"architecture"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        assert!(!body["result"]["isError"].as_bool().unwrap_or(true));
    }

    #[tokio::test]
    async fn test_submit_without_temper_fails() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"submit","arguments":{"name":"test","content":"hello"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_judge_without_temper_fails() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"judge","arguments":{"name":"test"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_submit_stores_artifact() {
        let (app, session_id) = initialized_app().await;
        // Temper as arm (brief artifact type)
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        )
        .await;
        // Submit a typed requirement
        let body_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "submit", "arguments": {"name": "auth", "description": "User authentication", "user_story": "As a user, I want to log in"}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &body_str, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "ok", "expected ok confirmation");
    }

    #[tokio::test]
    async fn test_judge_artifact_not_found() {
        let (app, session_id) = initialized_app().await;
        // Call temper first
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        )
        .await;
        // Judge with nonexistent name
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"judge","arguments":{"name":"nonexistent"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        // No AR engine in tests → auto-approve (never reaches file read)
        assert!(body.get("error").is_none());
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let verdict: Value = serde_json::from_str(text).unwrap();
        assert_eq!(verdict["verdict"], "approve");
    }

    #[tokio::test]
    async fn test_temper_prefix_unique_across_sessions() {
        // Both sessions share the same app/state so the uniqueness check can detect collisions
        let app = test_app();

        // Initialize session 1
        let resp = app.clone().oneshot(
            Request::builder()
                .method("POST").uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#))
                .unwrap(),
        ).await.unwrap();
        let session_id_1 = resp.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();

        // Initialize session 2
        let resp = app.clone().oneshot(
            Request::builder()
                .method("POST").uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#))
                .unwrap(),
        ).await.unwrap();
        let session_id_2 = resp.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();

        // Call temper on session 1
        let resp1 = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id_1,
        ).await;
        let body1 = body_json(resp1).await;
        let text1 = body1["result"]["content"][0]["text"].as_str().unwrap();

        // Call temper on session 2
        let resp2 = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"02"}}}"#,
            &session_id_2,
        ).await;
        let body2 = body_json(resp2).await;
        let text2 = body2["result"]["content"][0]["text"].as_str().unwrap();

        // Extract prefixes from the PREFIX: <code>\n\n header
        let prefix1 = text1.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap();
        let prefix2 = text2.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap();

        assert_ne!(prefix1, prefix2, "prefixes must be unique across sessions");
    }

    // --- Configure tool tests ---

    #[tokio::test]
    async fn test_configure_list() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"configure","arguments":{"action":"list","level":"project"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert!(parsed["principles"].is_array());
        assert!(parsed["modes"].is_array());
        assert_eq!(parsed["level"], "project");
    }

    #[tokio::test]
    async fn test_configure_unknown_action() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"configure","arguments":{"action":"bad","level":"project"}}}"#,
            &session_id,
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_configure_add_group() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();
        let args = json!({ "group": "my-rules", "active": true });
        let result = configure_add_group(json!(1), &args, config_dir);
        assert!(result.get("error").is_none(), "unexpected error: {:?}", result);
        let content = std::fs::read_to_string(config_dir.join("principles.toml")).unwrap();
        assert!(content.contains("[groups.my-rules]"));
    }

    #[tokio::test]
    async fn test_configure_add_principle() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();

        // Add group first
        let args = json!({ "group": "my-rules", "active": true });
        configure_add_group(json!(1), &args, config_dir);

        // Add principle
        let args = json!({ "group": "my-rules", "name": "SRP", "rule": "One thing", "ask": ["Is it one?"] });
        let result = configure_add_principle(json!(2), &args, config_dir);
        assert!(result.get("error").is_none(), "unexpected error: {:?}", result);

        let content = std::fs::read_to_string(config_dir.join("principles.toml")).unwrap();
        assert!(content.contains("[[groups.my-rules.principles]]"));
        assert!(content.contains("SRP"));
    }

    #[tokio::test]
    async fn test_configure_add_principle_no_group() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();

        let args = json!({ "group": "missing", "name": "SRP", "rule": "One thing" });
        let result = configure_add_principle(json!(1), &args, config_dir);
        assert_eq!(result["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_configure_activate_deactivate() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();

        // Add group active=true
        let args = json!({ "group": "solid", "active": true });
        configure_add_group(json!(1), &args, config_dir);

        // Deactivate
        let args = json!({ "group": "solid" });
        let result = configure_set_active(json!(2), &args, config_dir, false);
        assert!(result.get("error").is_none(), "deactivate failed: {:?}", result);

        // Read back and verify
        let content = std::fs::read_to_string(config_dir.join("principles.toml")).unwrap();
        let val: toml::Value = toml::from_str(&content).unwrap();
        let active = val["groups"]["solid"]["active"].as_bool().unwrap_or(true);
        assert!(!active);
    }

    #[tokio::test]
    async fn test_configure_add_mode() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();

        let args = json!({ "name": "data-pipeline", "budget": "standard" });
        let result = configure_add_mode(json!(1), &args, config_dir);
        assert!(result.get("error").is_none(), "add_mode failed: {:?}", result);

        let content = std::fs::read_to_string(config_dir.join("feldspar.toml")).unwrap();
        assert!(content.contains("[modes.data-pipeline]"));
    }

    #[tokio::test]
    async fn test_configure_add_mode_creates_agent() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();

        let args = json!({ "name": "data-pipeline", "budget": "standard" });
        configure_add_mode(json!(1), &args, config_dir);

        let agent_path = config_dir.join("agents/data-pipeline.toml");
        assert!(agent_path.exists(), "agent TOML not created");
        let content = std::fs::read_to_string(&agent_path).unwrap();
        assert!(content.contains("thinking_mode = \"data-pipeline\""));
    }

    #[tokio::test]
    async fn test_configure_remove_mode() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path();

        // Add then remove
        let args = json!({ "name": "data-pipeline", "budget": "standard" });
        configure_add_mode(json!(1), &args, config_dir);

        let args = json!({ "name": "data-pipeline" });
        let result = configure_remove_mode(json!(2), &args, config_dir);
        assert!(result.get("error").is_none(), "remove_mode failed: {:?}", result);

        let content = std::fs::read_to_string(config_dir.join("feldspar.toml")).unwrap();
        assert!(!content.contains("[modes.data-pipeline]"));
        assert!(!config_dir.join("agents/data-pipeline.toml").exists());
    }

    // --- Submit validation tests (Task 1) ---

    #[tokio::test]
    async fn test_submit_valid_brief() {
        let (app, session_id) = initialized_app().await;
        // Temper as arm (artifact_type=brief, thinking_mode=brainstorming)
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "Auth", "description": "User authentication",
                "user_story": "As a user, I want to log in"
            }}
        })).unwrap();
        let resp = post_mcp_with_session(app, &body_str, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "ok");
    }

    #[tokio::test]
    async fn test_submit_invalid_brief() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        // Missing required fields user_story → deserialization error
        let body_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "submit", "arguments": {"name": "X"}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &body_str, &session_id).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.to_lowercase().contains("invalid") || msg.to_lowercase().contains("missing"), "error: {}", msg);
    }

    #[tokio::test]
    async fn test_build_temper_omits_submit_in_prompt() {
        // Build has artifact_type="code" — no typed submit/revise/remove.
        // The temper prompt should not document submit/revise/remove but
        // should document fetch and judge.
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("## Available Tools"));
        // Isolate the Available Tools section up to the next section header.
        let after_header = text.split("## Available Tools").nth(1).unwrap_or("");
        let tools_block = after_header.split("\n## ").next().unwrap_or(after_header);
        assert!(!tools_block.contains("`submit`"), "build must not document submit");
        assert!(!tools_block.contains("`revise`"), "build must not document revise");
        assert!(!tools_block.contains("`remove`"), "build must not document remove");
        assert!(tools_block.contains("`fetch`"), "build must document fetch. block: {}", tools_block);
        assert!(tools_block.contains("`judge`"), "build is ar_gated so must document judge");
    }

    // --- Temper prefix tests (Task 2) ---

    #[tokio::test]
    async fn test_temper_with_prefix_reuses() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"solve","prefix":"bf7k"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("PREFIX: bf7k"), "expected PREFIX: bf7k in: {}", &text[..80.min(text.len())]);
    }

    #[tokio::test]
    async fn test_temper_without_prefix_generates() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let first_line = text.lines().next().unwrap();
        assert!(first_line.starts_with("PREFIX: "), "expected PREFIX line");
        let generated = first_line.strip_prefix("PREFIX: ").unwrap();
        assert_eq!(generated.len(), 4, "generated prefix should be 4 chars");
    }

    #[tokio::test]
    async fn test_temper_empty_prefix_generates() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm","prefix":""}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let first_line = text.lines().next().unwrap();
        let generated = first_line.strip_prefix("PREFIX: ").unwrap();
        assert_eq!(generated.len(), 4, "empty prefix should generate new 4-char prefix");
    }

    // --- Fetch tool tests (Task 3) ---

    #[tokio::test]
    async fn test_fetch_existing_artifact() {
        let (app, session_id) = initialized_app().await;

        // Temper as arm to get a prefix
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        // Submit a typed requirement
        let submit_body = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "X", "description": "Y", "user_story": "Z"
            }}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit_body, &session_id).await;

        // Temper as solve with same prefix to get access
        let temper2 = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "temper", "arguments": {"role": "solve", "prefix": prefix}}
        })).unwrap();
        post_mcp_with_session(app.clone(), &temper2, &session_id).await;

        // Fetch the brief
        let fetch_body = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "fetch", "arguments": {"prefix": prefix, "type": "brief"}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &fetch_body, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let returned = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(returned.contains("requirements"), "expected brief content: {}", returned);
    }

    #[tokio::test]
    async fn test_fetch_not_found() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fetch","arguments":{"prefix":"zzzz","type":"brief"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("not found"), "expected not found in: {}", msg);
    }

    #[tokio::test]
    async fn test_fetch_unknown_type() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fetch","arguments":{"prefix":"test","type":"bad"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("Unknown artifact type"), "expected unknown type error: {}", msg);
    }

    #[tokio::test]
    async fn test_fetch_without_temper() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fetch","arguments":{"prefix":"test","type":"brief"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("temper"), "expected temper error: {}", msg);
    }

    // --- Task 1 tests: Session role/group fields ---

    #[tokio::test]
    async fn test_session_new_fields_default_none() {
        let app = test_app();
        let resp = post_mcp(
            app,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#,
        ).await;
        // Session is created; we can't read it directly in this test, but we verify
        // tools/list returns temper (meaning has_role=false, i.e. role is None)
        let session_id = resp.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();
        let app2 = test_app();
        let resp2 = app2.clone().oneshot(
            Request::builder()
                .method("POST").uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#))
                .unwrap(),
        ).await.unwrap();
        let sid2 = resp2.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();
        let resp3 = post_mcp_with_session(app2, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &sid2).await;
        let body = body_json(resp3).await;
        let names: Vec<_> = body["result"]["tools"].as_array().unwrap()
            .iter().map(|t| t["name"].as_str().unwrap()).collect();
        // temper visible means role is None (default)
        assert!(names.contains(&"temper"), "temper should be visible when role is None");
        let _ = session_id; // suppress unused warning
    }

    // --- Task 2 tests: Temper group validation ---

    #[tokio::test]
    async fn test_temper_build_sets_role_and_group() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        // Verify role is set: tools/list should hide temper
    }

    #[tokio::test]
    async fn test_temper_build_rejects_invalid_group() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"1"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("two-digit"), "expected two-digit in error: {}", msg);
    }

    #[tokio::test]
    async fn test_temper_build_rejects_traversal_group() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"../"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_temper_bugfest_sets_role_no_group() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"bugfest"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        // bugfest succeeds without group
    }

    #[tokio::test]
    async fn test_temper_creates_change_file_build() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01","prefix":"ab12"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let change_file = init::data_dir("test")
            .join("artifacts/changes/implementation/ab12/01-changes.toml");
        assert!(change_file.exists(), "expected change file at {:?}", change_file);
    }

    #[tokio::test]
    async fn test_temper_creates_change_file_bugfest() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"bugfest","prefix":"cd34"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let change_file = init::data_dir("test")
            .join("artifacts/changes/debugging/cd34/changes.toml");
        assert!(change_file.exists(), "expected change file at {:?}", change_file);
    }

    #[tokio::test]
    async fn test_tools_list_shows_temper_always() {
        // temper is always in tools/list (including after being called) —
        // dynamic hiding doesn't work with one-shot discovery clients.
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let names: Vec<_> = body["result"]["tools"].as_array().unwrap()
            .iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"temper"));
    }

    // --- Task 3 tests: GET /session/:id ---

    #[tokio::test]
    async fn test_session_lookup_returns_info() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01","prefix":"ab12"}}}"#,
            &session_id,
        ).await;
        let resp = app.oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{}", session_id))
                .body(Body::empty())
                .unwrap(),
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["prefix"], "ab12");
        assert_eq!(body["group"], "01");
        assert_eq!(body["role"], "build");
    }

    #[tokio::test]
    async fn test_session_lookup_not_found() {
        let app = test_app();
        let resp = app.oneshot(
            Request::builder()
                .method("GET")
                .uri("/session/nonexistent-id-that-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- Task 4 tests: Cleanup + per-prefix deletion ---

    #[tokio::test]
    async fn test_cleanup_deletes_orphaned_prefix_dir() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01","prefix":"cl01"}}}"#,
            &session_id,
        ).await;

        let change_dir = init::data_dir("test")
            .join("artifacts/changes/implementation/cl01");
        assert!(change_dir.exists(), "change dir should exist after temper");

        // Get state and expire the session manually
        let state = Arc::new(McpState::new(
            ThinkingServer::new(test_config(), None, None, Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())), None),
            crate::agents::load_agents("test"),
            None,
            "test".into(),
            0,
        ));
        {
            let mut sessions = state.sessions.write().await;
            let expired = Session {
                id: "exp1".into(),
                initialized: true,
                created_at: 0,
                last_activity: 0, // already expired
                prefix: Some("cl01".into()),
                thinking_mode: None,
                artifact_type: None,
                ar_gated: false,
                judge_cycle: 0,
                role: Some("build".into()),
                group: Some("01".into()),
            };
            sessions.insert("exp1".into(), expired);
        }
        // Manually run one cleanup cycle
        let cutoff = now_millis() - SESSION_TTL_MS;
        let orphaned: Vec<String> = {
            let mut sessions = state.sessions.write().await;
            let evicted: Vec<String> = sessions.values()
                .filter(|s| s.last_activity <= cutoff)
                .filter_map(|s| s.prefix.clone())
                .collect();
            sessions.retain(|_, s| s.last_activity > cutoff);
            evicted.into_iter()
                .filter(|p| !sessions.values().any(|s| s.prefix.as_deref() == Some(p.as_str())))
                .collect()
        };
        for prefix in orphaned {
            let base = init::data_dir(&state.project_name);
            for mode in &["implementation", "debugging"] {
                let _ = std::fs::remove_dir_all(base.join("artifacts/changes").join(mode).join(&prefix));
            }
        }

        assert!(!change_dir.exists(), "change dir should be deleted after cleanup");
    }

    #[tokio::test]
    async fn test_cleanup_preserves_active_prefix_dir() {
        let state = Arc::new(McpState::new(
            ThinkingServer::new(test_config(), None, None, Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())), None),
            crate::agents::load_agents("test"),
            None,
            "test".into(),
            0,
        ));

        // Create the prefix dir manually
        let change_dir = init::data_dir("test").join("artifacts/changes/implementation/sh01");
        std::fs::create_dir_all(&change_dir).unwrap();

        {
            let mut sessions = state.sessions.write().await;
            // Expired session with prefix sh01
            sessions.insert("exp".into(), Session {
                id: "exp".into(), initialized: true, created_at: 0, last_activity: 0,
                prefix: Some("sh01".into()), thinking_mode: None, artifact_type: None,
                ar_gated: false, judge_cycle: 0, role: Some("build".into()), group: Some("01".into()),
            });
            // Active session with same prefix
            sessions.insert("active".into(), Session {
                id: "active".into(), initialized: true, created_at: now_millis(),
                last_activity: now_millis(),
                prefix: Some("sh01".into()), thinking_mode: None, artifact_type: None,
                ar_gated: false, judge_cycle: 0, role: Some("build".into()), group: Some("02".into()),
            });
        }

        let cutoff = now_millis() - SESSION_TTL_MS;
        let orphaned: Vec<String> = {
            let mut sessions = state.sessions.write().await;
            let evicted: Vec<String> = sessions.values()
                .filter(|s| s.last_activity <= cutoff)
                .filter_map(|s| s.prefix.clone())
                .collect();
            sessions.retain(|_, s| s.last_activity > cutoff);
            evicted.into_iter()
                .filter(|p| !sessions.values().any(|s| s.prefix.as_deref() == Some(p.as_str())))
                .collect()
        };
        assert!(orphaned.is_empty(), "prefix shared with active session should not be orphaned");
        assert!(change_dir.exists(), "change dir should be preserved when active session exists");
    }

    // --- Task 5 tests: sweep_orphaned_changes + init dirs ---

    #[test]
    fn test_sweep_preserves_young_dirs() {
        // Create a dir under the real data dir (young — just created, won't be swept)
        let young_dir = init::data_dir("test")
            .join("artifacts/changes/implementation/young");
        std::fs::create_dir_all(&young_dir).unwrap();
        sweep_orphaned_changes("test");
        assert!(young_dir.exists(), "young dir should not be swept");
    }

    #[test]
    fn test_sweep_deletes_old_dirs() {
        // Can't backdate mtime in stable std without external crate.
        // Verify that a freshly-created dir is preserved by sweep (threshold >> actual age).
        let fresh_dir = init::data_dir("test")
            .join("artifacts/changes/implementation/fresh");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        sweep_orphaned_changes("test");
        assert!(fresh_dir.exists(), "freshly created dir should not be deleted by sweep");
    }

    #[test]
    fn test_init_creates_change_dirs() {
        // Verify that create_data_dirs creates change dirs (uses actual home dir like all other tests)
        crate::init::create_data_dirs("test").unwrap();
        assert!(init::data_dir("test").join("artifacts/changes/implementation").exists());
        assert!(init::data_dir("test").join("artifacts/changes/debugging").exists());
    }

    // --- Integration tests: full flow ---

    #[tokio::test]
    async fn test_full_flow_build_agent() {
        let (app, session_id) = initialized_app().await;

        // Temper as build agent with explicit prefix and group
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01","prefix":"t3st"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "temper error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("build agent"), "expected agent prompt: {}", text);

        // Change file must exist after temper
        let change_file = init::data_dir("test")
            .join("artifacts/changes/implementation/t3st/01-changes.toml");
        assert!(change_file.exists(), "change file must exist after temper: {:?}", change_file);

        // Write a mock change entry to simulate the hook
        let entry = "\n[[changes]]\ntimestamp = 1234567890\nfile = \"src/main.rs\"\ndiff = \"\"\"\n-old line\n+new line\n\"\"\"\n";
        std::fs::write(&change_file, entry).unwrap();

        // build has artifact_type=code — no typed submit tool exposed
        // Judge auto-approves (no AR engine in tests)
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"judge","arguments":{"name":"plan"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "judge error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let verdict: Value = serde_json::from_str(text).unwrap();
        assert_eq!(verdict["verdict"], "approve");

        // Change file still contains our entry (not deleted by judge)
        let content = std::fs::read_to_string(&change_file).unwrap();
        assert!(content.contains("src/main.rs"), "change file should contain written entry");
    }

    #[tokio::test]
    async fn test_full_flow_bugfest_agent() {
        // Clean up artifact from previous runs
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let artifact_path = std::path::PathBuf::from(home)
            .join(".feldspar/data/test/artifacts/debugging/b4gs.toml");
        let _ = std::fs::remove_file(&artifact_path);

        let (app, session_id) = initialized_app().await;

        // Temper as bugfest agent
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"bugfest","prefix":"b4gs"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "temper error: {:?}", body);

        // Change file must exist at debugging path (no group)
        let change_file = init::data_dir("test")
            .join("artifacts/changes/debugging/b4gs/changes.toml");
        assert!(change_file.exists(), "bugfest change file must exist: {:?}", change_file);

        // Session must have role=bugfest, group=None (verify via /session endpoint)
        let resp = app.clone().oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{}", session_id))
                .body(Body::empty())
                .unwrap(),
        ).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let session_info: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(session_info["role"], "bugfest");
        assert!(session_info["group"].is_null(), "bugfest must have no group");

        // Write mock change entry
        let entry = "\n[[changes]]\ntimestamp = 9999\nfile = \"src/bug.rs\"\ndiff = \"\"\"\n-bad\n+good\n\"\"\"\n";
        std::fs::write(&change_file, entry).unwrap();

        // Submit a valid diagnosis artifact (typed fields)
        let submit_body = serde_json::json!({
            "jsonrpc": "2.0", "id": 5,
            "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "symptom": "crash on startup",
                "root_cause": "null pointer in init",
                "evidence": ["src/main.rs:10"],
                "fix": "add null check",
                "files_changed": ["src/main.rs"]
            }}
        });
        let resp = post_mcp_with_session(
            app.clone(),
            &submit_body.to_string(),
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "submit error: {:?}", body);

        // Judge auto-approves
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"judge","arguments":{"name":"debug"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "judge error: {:?}", body);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let verdict: Value = serde_json::from_str(text).unwrap();
        assert_eq!(verdict["verdict"], "approve");

        // Change file still readable
        let content = std::fs::read_to_string(&change_file).unwrap();
        assert!(content.contains("src/bug.rs"));
    }

    // --- Integration tests: multi-agent parallel ---

    #[tokio::test]
    async fn test_parallel_build_agents_separate_files() {
        let app = test_app();

        // Initialize session A
        let resp = app.clone().oneshot(
            Request::builder()
                .method("POST").uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#))
                .unwrap(),
        ).await.unwrap();
        let session_a = resp.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();

        // Initialize session B
        let resp = app.clone().oneshot(
            Request::builder()
                .method("POST").uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"0.1"}}}"#))
                .unwrap(),
        ).await.unwrap();
        let session_b = resp.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();

        // Temper session A: prefix=para, group=01
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"01","prefix":"para"}}}"#,
            &session_a,
        ).await;

        // Temper session B: same prefix, group=02
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"02","prefix":"para"}}}"#,
            &session_b,
        ).await;

        let base = init::data_dir("test").join("artifacts/changes/implementation/para");

        // Each session has its own change file
        let file_a = base.join("01-changes.toml");
        let file_b = base.join("02-changes.toml");
        assert!(file_a.exists(), "session A change file must exist: {:?}", file_a);
        assert!(file_b.exists(), "session B change file must exist: {:?}", file_b);

        // Write distinct change entries to each file
        std::fs::write(&file_a, "[[changes]]\ntimestamp=1\nfile=\"a.rs\"\ndiff=\"\"\"\n+a\n\"\"\"\n").unwrap();
        std::fs::write(&file_b, "[[changes]]\ntimestamp=2\nfile=\"b.rs\"\ndiff=\"\"\"\n+b\n\"\"\"\n").unwrap();

        // Judge from session A returns auto-approve
        let resp_a = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"judge","arguments":{"name":"plan"}}}"#,
            &session_a,
        ).await;
        let body_a = body_json(resp_a).await;
        assert!(body_a.get("error").is_none(), "judge A error: {:?}", body_a);

        // Judge from session B returns auto-approve
        let resp_b = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"judge","arguments":{"name":"plan"}}}"#,
            &session_b,
        ).await;
        let body_b = body_json(resp_b).await;
        assert!(body_b.get("error").is_none(), "judge B error: {:?}", body_b);

        // Each file retains its own content
        let content_a = std::fs::read_to_string(&file_a).unwrap();
        let content_b = std::fs::read_to_string(&file_b).unwrap();
        assert!(content_a.contains("a.rs"), "file A should contain A's changes");
        assert!(content_b.contains("b.rs"), "file B should contain B's changes");
        assert!(!content_a.contains("b.rs"), "file A must not contain B's changes");
        assert!(!content_b.contains("a.rs"), "file B must not contain A's changes");
    }

    // --- Edge case and validation tests ---

    #[tokio::test]
    async fn test_non_build_role_no_change_file() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"solve","prefix":"s0lv"}}}"#,
            &session_id,
        ).await;

        let impl_dir = init::data_dir("test").join("artifacts/changes/implementation/s0lv");
        let debug_dir = init::data_dir("test").join("artifacts/changes/debugging/s0lv");
        assert!(!impl_dir.exists(), "solve role must not create implementation change dir");
        assert!(!debug_dir.exists(), "solve role must not create debugging change dir");
    }

    #[tokio::test]
    async fn test_group_validation_rejects_single_digit() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"1"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(
            body["error"]["message"].as_str().unwrap().contains("two-digit"),
            "error must mention two-digit: {:?}", body["error"]["message"]
        );
    }

    #[tokio::test]
    async fn test_group_validation_rejects_three_digits() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"temper","arguments":{"role":"build","group":"001"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(
            body["error"]["message"].as_str().unwrap().contains("two-digit"),
            "error must mention two-digit: {:?}", body["error"]["message"]
        );
    }


    #[test]
    fn test_triple_quote_escaping_in_toml() {
        // Simulate the escaping logic used in record_change()
        let raw_diff = "-old \"\"\" line\n+new \"\"\" line";
        let safe_diff = raw_diff.replace("\"\"\"", "\"\"\\\"");

        let entry = format!(
            "[[changes]]\ntimestamp = 1234\nfile = \"test.rs\"\ndiff = \"\"\"\n{}\n\"\"\"\n",
            safe_diff
        );

        // The resulting TOML must parse without error
        let parsed: Result<toml::Value, _> = toml::from_str(&entry);
        assert!(parsed.is_ok(), "escaped TOML must parse: {:?}\n---\n{}", parsed.err(), entry);

        // The parsed diff must contain the original triple-quote characters
        let changes = parsed.unwrap();
        let diff_val = changes["changes"][0]["diff"].as_str().unwrap();
        assert!(diff_val.contains("\"\"\""), "parsed diff must restore triple quotes");
    }

    // --- Typed artifact submission tests (Agent 2) ---

    #[test]
    fn test_toml_key_for_all_types() {
        assert_eq!(toml_key_for("brief"), ("requirements", Some("name")));
        assert_eq!(toml_key_for("design"), ("modules", Some("name")));
        assert_eq!(toml_key_for("execution_plan"), ("tasks", Some("name")));
        assert_eq!(toml_key_for("diagnosis"), ("diagnosis", None));
        assert_eq!(toml_key_for("validation_report"), ("claims", Some("number")));
    }

    #[test]
    fn test_extract_key_string() {
        let result = extract_key(&json!({"name": "auth"}), Some("name"));
        assert_eq!(result, Ok(toml::Value::String("auth".into())));
    }

    #[test]
    fn test_extract_key_integer() {
        let result = extract_key(&json!({"number": 1}), Some("number"));
        assert_eq!(result, Ok(toml::Value::Integer(1)));
    }

    #[test]
    fn test_extract_key_missing() {
        let result = extract_key(&json!({}), Some("name"));
        assert!(result.is_err());
    }

    #[test]
    fn test_find_unit_index_string_key() {
        let arr = vec![
            toml::Value::Table({
                let mut t = toml::map::Map::new();
                t.insert("name".into(), toml::Value::String("a".into()));
                t
            }),
            toml::Value::Table({
                let mut t = toml::map::Map::new();
                t.insert("name".into(), toml::Value::String("b".into()));
                t
            }),
        ];
        assert_eq!(find_unit_index(&arr, "name", &toml::Value::String("b".into())), Some(1));
    }

    #[test]
    fn test_find_unit_index_integer_key() {
        let arr = vec![
            toml::Value::Table({
                let mut t = toml::map::Map::new();
                t.insert("number".into(), toml::Value::Integer(1));
                t
            }),
            toml::Value::Table({
                let mut t = toml::map::Map::new();
                t.insert("number".into(), toml::Value::Integer(2));
                t
            }),
        ];
        assert_eq!(find_unit_index(&arr, "number", &toml::Value::Integer(2)), Some(1));
    }

    #[test]
    fn test_find_unit_index_not_found() {
        let arr = vec![toml::Value::Table({
            let mut t = toml::map::Map::new();
            t.insert("name".into(), toml::Value::String("x".into()));
            t
        })];
        assert_eq!(find_unit_index(&arr, "name", &toml::Value::String("y".into())), None);
    }

    #[tokio::test]
    async fn test_temper_response_documents_submit_shape_for_arm() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        // arm → brief → submit shape documented in the temper prompt
        assert!(text.contains("## Available Tools"), "temper must include tools section");
        assert!(text.contains("submit"));
        assert!(text.contains("user_story"), "arm temper must document brief unit shape");
    }

    #[tokio::test]
    async fn test_submit_requirement_creates_file() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        let body_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "auth", "description": "Authentication", "user_story": "As a user"
            }}
        })).unwrap();
        let resp = post_mcp_with_session(app, &body_str, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        assert!(artifact_file.exists(), "artifact file must exist");
        let content = std::fs::read_to_string(&artifact_file).unwrap();
        assert!(content.contains("[[requirements]]"));
        assert!(content.contains("auth"));
    }

    #[tokio::test]
    async fn test_submit_duplicate_errors() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        let submit = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "auth", "description": "Auth", "user_story": "As user"
            }}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit, &session_id).await;

        let resp = post_mcp_with_session(app, &submit, &session_id).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"].as_str().unwrap().contains("already exists"));
    }

    #[tokio::test]
    async fn test_submit_diagnosis_singleton() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"bugfest"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/debugging")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        let body_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "symptom": "crash", "root_cause": "null ptr",
                "evidence": ["src/main.rs:1"], "fix": "add check",
                "files_changed": ["src/main.rs"]
            }}
        })).unwrap();
        let resp = post_mcp_with_session(app, &body_str, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "unexpected error: {:?}", body);
        let content = std::fs::read_to_string(&artifact_file).unwrap();
        assert!(content.contains("[diagnosis]"));
    }

    #[tokio::test]
    async fn test_submit_invalid_fields() {
        let (app, session_id) = initialized_app().await;
        post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        // Missing user_story → deserialization error
        let body_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {"name": "X", "description": "Y"}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &body_str, &session_id).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_revise_requirement() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        // Submit
        let submit = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "auth", "description": "Old description", "user_story": "As user"
            }}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit, &session_id).await;

        // Revise
        let revise = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "revise", "arguments": {
                "name": "auth", "description": "New description", "user_story": "As user"
            }}
        })).unwrap();
        let resp = post_mcp_with_session(app, &revise, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "revise error: {:?}", body);
        let content = std::fs::read_to_string(&artifact_file).unwrap();
        assert!(content.contains("New description"));
        assert!(!content.contains("Old description"));
    }

    #[tokio::test]
    async fn test_revise_not_found() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        // Submit one requirement
        let submit = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "existing", "description": "D", "user_story": "S"
            }}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit, &session_id).await;

        // Try to revise a nonexistent one
        let revise = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "revise", "arguments": {
                "name": "nonexistent", "description": "D", "user_story": "S"
            }}
        })).unwrap();
        let resp = post_mcp_with_session(app, &revise, &session_id).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_revise_no_artifact() {
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        let revise = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "revise", "arguments": {
                "name": "auth", "description": "D", "user_story": "S"
            }}
        })).unwrap();
        let resp = post_mcp_with_session(app, &revise, &session_id).await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"].as_str().unwrap().contains("No artifact to revise"));
    }

    #[tokio::test]
    async fn test_remove_requirement() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        // Submit 2 requirements
        for name in ["keep", "remove_me"] {
            let body_str = serde_json::to_string(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "submit", "arguments": {
                    "name": name, "description": "D", "user_story": "S"
                }}
            })).unwrap();
            post_mcp_with_session(app.clone(), &body_str, &session_id).await;
        }

        // Remove one
        let remove = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "remove", "arguments": {"name": "remove_me"}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &remove, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "remove error: {:?}", body);

        let content = std::fs::read_to_string(&artifact_file).unwrap();
        assert!(content.contains("keep"), "kept requirement must remain");
        assert!(!content.contains("remove_me"), "removed requirement must be gone");
    }

    #[tokio::test]
    async fn test_remove_diagnosis_truncates() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"bugfest"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/debugging")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        // Submit diagnosis
        let submit = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "symptom": "crash", "root_cause": "null",
                "evidence": ["src/main.rs:1"], "fix": "fix",
                "files_changed": ["src/main.rs"]
            }}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit, &session_id).await;

        // Remove → truncates
        let remove = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "remove", "arguments": {}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &remove, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "remove error: {:?}", body);
        assert!(artifact_file.exists(), "file must still exist after truncate");
        let content = std::fs::read_to_string(&artifact_file).unwrap();
        assert!(content.is_empty(), "file must be empty after diagnosis remove");
    }

    #[tokio::test]
    async fn test_remove_then_submit() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"bugfest"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/debugging")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        let diag = json!({
            "symptom": "crash", "root_cause": "null",
            "evidence": ["src/main.rs:1"], "fix": "fix",
            "files_changed": ["src/main.rs"]
        });

        let submit = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": diag}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit, &session_id).await;

        let remove = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "remove", "arguments": {}}
        })).unwrap();
        post_mcp_with_session(app.clone(), &remove, &session_id).await;

        // Submit again should succeed after truncation
        let resp = post_mcp_with_session(app, &submit, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "second submit after remove error: {:?}", body);
    }

    #[tokio::test]
    async fn test_fetch_after_typed_submit() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let (app, session_id) = initialized_app().await;

        // Temper as arm
        let resp = post_mcp_with_session(
            app.clone(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"temper","arguments":{"role":"arm"}}}"#,
            &session_id,
        ).await;
        let body = body_json(resp).await;
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let prefix = text.lines().next().unwrap().strip_prefix("PREFIX: ").unwrap().to_owned();

        let artifact_file = std::path::PathBuf::from(&home)
            .join(".feldspar/data/test/artifacts/brainstorming")
            .join(format!("{}.toml", prefix));
        let _ = std::fs::remove_file(&artifact_file);

        // Submit requirement
        let submit = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "submit", "arguments": {
                "name": "auth", "description": "Auth module", "user_story": "As a user"
            }}
        })).unwrap();
        post_mcp_with_session(app.clone(), &submit, &session_id).await;

        // Fetch brief
        let fetch = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "fetch", "arguments": {"prefix": prefix, "type": "brief"}}
        })).unwrap();
        let resp = post_mcp_with_session(app, &fetch, &session_id).await;
        let body = body_json(resp).await;
        assert!(body.get("error").is_none(), "fetch error: {:?}", body);
        let returned = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(returned.contains("requirements"), "expected requirements in: {}", returned);
        assert!(returned.contains("auth"));
    }
}
