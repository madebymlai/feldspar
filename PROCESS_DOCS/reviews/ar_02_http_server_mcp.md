# Adversarial Review: HTTP Server + MCP Protocol (02-http-server-mcp.md)

## Summary

Design is architecturally sound but has 4 critical gaps: missing trace_id field (breaks multi-thought flow), missing Origin header validation (spec MUST), no wire response type (Claude gets wrong format), and underspecified argument extraction path. After amendments, ready for `/breakdown`.

**Reviewers**: ar-o (Opus), ar-k (Kimi/Valence), ar-glm5 (GLM-5.1)

## Critical (Must Address)

### Missing trace_id in ThoughtInput breaks multi-thought traces
**Flagged by**: ar-o, ar-k, ar-glm5 (3/3)  |  **Confidence**: High

ThoughtInput has no `trace_id` field. The design's per-thought flow says "lookup Trace by trace_id from arguments" but the type has no such field. After thought 1 creates a trace and returns `traceId`, the client has no way to send it back. Every subsequent thought would create a new trace.

| Factor | Assessment |
|--------|------------|
| Severity | System failure — multi-thought traces impossible |
| Probability | Guaranteed — every trace with >1 thought |
| Remediation Cost | Simple — add `trace_id: Option<String>` to ThoughtInput, update inputSchema |
| Reversibility | Load-bearing — must decide now |
| Context Fit | Core functionality, blocks basic usage |

**Mitigation**: Add `pub trace_id: Option<String>` to ThoughtInput with `#[serde(default)]`. Add `traceId` to the tool's inputSchema. If absent on `thought_number > 1`, return JSON-RPC -32602 error. Requires updating issue #1's ThoughtInput type.

### Missing Origin header validation — DNS rebinding attack
**Flagged by**: ar-o, ar-k, ar-glm5 (3/3)  |  **Confidence**: High

MCP spec states: "Servers MUST validate the Origin header on all incoming connections to prevent DNS rebinding attacks." The design makes zero mention of Origin validation.

| Factor | Assessment |
|--------|------------|
| Severity | Security vulnerability — DNS rebinding |
| Probability | Low for localhost but spec MUST |
| Remediation Cost | Simple — ~15 line axum middleware |
| Reversibility | Fixable later but spec compliance is day-1 |
| Context Fit | Spec MUST, cannot ship without it |

**Mitigation**: Add Origin validation middleware. Accept requests with no Origin (non-browser clients), `Origin: http://localhost:*`, `Origin: http://127.0.0.1:*`. Reject all others with 403. Add tests.

### No wire response type defined — ThoughtResult != wire format
**Flagged by**: ar-o, ar-glm5 (2/3)  |  **Confidence**: High

Issue #1 explicitly states: "The flat wire response is NOT ThoughtResult directly." The upstream design doc defines a flat format with `traceId`, `thoughtNumber`, `trajectory` (not `mlTrajectory`), `driftDetected` (not `mlDrift`), plus echo-back fields. The design serializes ThoughtResult directly — wrong shape.

| Factor | Assessment |
|--------|------------|
| Severity | System failure — Claude receives wrong-shaped response |
| Probability | Guaranteed — types don't match |
| Remediation Cost | Moderate — define WireResponse struct, build merge logic |
| Reversibility | Load-bearing — wire format is the API contract |
| Context Fit | Issue #1 explicitly deferred this to this issue |

**Mitigation**: Define `WireResponse` struct (Serialize, camelCase) merging: echo-backs from ThoughtInput, trace metadata, renamed ThoughtResult fields. Build it in `process_thought()`.

### tools/call argument extraction path underspecified
**Flagged by**: ar-glm5 (1/3)  |  **Confidence**: High

MCP tools/call sends `params: { name: "sequentialthinking", arguments: {...} }`. Design says "deserialize arguments as ThoughtInput" but doesn't specify extracting `params.arguments` vs `params`. An implementer deserializing `params` directly will fail because it also contains `name` and `_meta`.

| Factor | Assessment |
|--------|------------|
| Severity | System failure if implemented wrong |
| Probability | Guaranteed without explicit spec |
| Remediation Cost | Trivial — one sentence in design |
| Reversibility | Trivial |
| Context Fit | Must be in the design to prevent implementer confusion |

**Mitigation**: Specify: validate `params.name == "sequentialthinking"`, then deserialize `params.arguments` as ThoughtInput.

## Recommended (High Value)

### Initialize must parse client protocolVersion
**Flagged by**: ar-o, ar-glm5  |  **Confidence**: Medium

MCP lifecycle requires version negotiation. Design hardcodes response version without reading client's request.

| Factor | Assessment |
|--------|------------|
| Severity | Degraded UX — silent protocol mismatch |
| Probability | Low — Claude Code likely sends a supported version |
| Remediation Cost | Simple — parse params, validate version |
| Reversibility | Fixable later |
| Context Fit | Spec requirement |

**Mitigation**: Parse `params.protocolVersion`. Support `2025-11-25`. If client sends unsupported version, return JSON-RPC error.

### Use protocol version 2025-11-25
**Flagged by**: ar-k, ar-glm5  |  **Confidence**: Medium

Design uses `2025-03-26`. Current spec version is `2025-11-25`.

**Mitigation**: Change to `2025-11-25`. Implement version negotiation.

### JSON-RPC errors should return HTTP 200
**Flagged by**: ar-k  |  **Confidence**: Medium

JSON-RPC 2.0 over HTTP: protocol errors (method not found, invalid params) return HTTP 200 with error in body. HTTP 4xx only for transport-level failures.

**Mitigation**: Return HTTP 200 for all JSON-RPC responses (including errors). HTTP 400 only for malformed HTTP or missing session header.

### Update CLAUDE.md — remove "stdio only"
**Flagged by**: ar-glm5  |  **Confidence**: High

CLAUDE.md §7 says "MCP transport: stdio only (no network exposure)." This contradicts the HTTP design.

**Mitigation**: Change to "MCP transport: HTTP on localhost only (127.0.0.1, no external exposure)."

### Session TTL with background sweep
**Flagged by**: ar-o, ar-k, ar-glm5 (3/3)  |  **Confidence**: Medium

No eviction logic. Daemon mode = indefinite uptime = unbounded session growth.

**Mitigation**: Add TTL (e.g., 30 min inactivity). Background task: `sessions.retain(|_, s| s.last_activity > cutoff)` on 60-second interval.

## Noted (Awareness)

- **Daemon mode**: Add stderr→log file redirect, `setsid()` for proper detach, PID file for lifecycle management
- **Batch handling edge cases**: Empty batch → -32600 error, all-notifications batch → 202, mixed batch → responses for requests only
- **Accept header validation**: Validate includes `application/json`, return 406 if not
- **Unix-only signal handling**: Add `#[cfg(unix)]` for `SignalKind::terminate()`, fall back to `ctrl_c()` on other platforms
- **McpState / ThinkingServer relationship**: Document that sessions are transport-level, traces are application-level
- **MCP-Protocol-Version header**: Log but don't enforce for now
- **Content-Type validation on POST**: Ensure axum returns JSON-RPC error (not default rejection) for wrong Content-Type

## Recommendation

[x] REVISE — 4 Critical issues require design amendments before `/breakdown`
[ ] PROCEED
