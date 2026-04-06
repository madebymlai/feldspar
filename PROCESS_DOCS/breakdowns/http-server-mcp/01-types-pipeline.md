# Build Agent 1: Types + Pipeline Extensions

## Dependencies

None (parallel) — extends existing `src/thought.rs` from issue #1.

## Overview

- **Objective**: Extend issue #1's types to support the MCP server. Add `trace_id` to ThoughtInput, define the `WireResponse` wire format, add `Trace::new()` and `ThinkingServer::process_thought()` no-op pipeline.
- **Scope**:
  - Includes: `src/thought.rs` modifications only
  - Excludes: No MCP protocol, no HTTP server, no CLI, no `src/mcp.rs`, no `src/main.rs`
- **Dependencies**:
  - Existing code: `src/thought.rs` (from issue #1, already implemented)
  - Existing code: `src/config.rs` (Config type, already implemented)
  - Crates already available: `serde`, `serde_json`, `uuid`, `tokio`
- **Estimated Complexity**: Low-Medium — type additions + one method with no-op logic

## Technical Approach

### What Changes in thought.rs

1. **ThoughtInput** gets a new field: `trace_id: Option<String>`
2. **WireResponse** — new struct that's the actual MCP tool output (NOT ThoughtResult)
3. **Trace::new()** — constructor generating UUID + timestamp
4. **ThinkingServer::process_thought()** — no-op pipeline that creates/looks up trace, builds WireResponse

### Data Flow

```
ThoughtInput arrives (with optional trace_id)
  → if thought_number == 1 && trace_id.is_none(): Trace::new()
  → if trace_id.is_some(): lookup in traces HashMap
  → append ThoughtRecord to trace
  → build WireResponse from input echo-backs + trace metadata + default ThoughtResult
  → return WireResponse
```

### WireResponse Design

The MCP wire format is a flat JSON object merging three sources:
- **Echo-backs** from ThoughtInput: thoughtNumber, totalThoughts, nextThoughtNeeded
- **Trace metadata**: traceId, branches, thoughtHistoryLength
- **ThoughtResult fields** (renamed where needed): warnings, alerts, trajectory (not mlTrajectory), driftDetected (not mlDrift)

---

## Task Breakdown

### Task 1: Add trace_id to ThoughtInput

- **Description**: Add `trace_id: Option<String>` field to ThoughtInput for multi-thought trace correlation.
- **Acceptance Criteria**:
  - [ ] `trace_id` field added to ThoughtInput with `#[serde(default)]`
  - [ ] Existing tests still pass (`cargo test`)
  - [ ] New field deserializes from camelCase `traceId` in JSON
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: None
- **Code Example**:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct ThoughtInput {
      pub trace_id: Option<String>,  // ADD THIS — first field for visibility
      pub thought: String,
      pub thought_number: u32,
      // ... rest unchanged
  }
  ```
- **Test Cases** (file: `src/thought.rs` inline `#[cfg(test)]`):
  - **`test_thought_input_with_trace_id`**: Deserialize JSON with `"traceId": "abc-123"`. Assert `input.trace_id == Some("abc-123".into())`.
  - **`test_thought_input_without_trace_id`**: Deserialize JSON without traceId field. Assert `input.trace_id.is_none()`. (This is the existing `test_thought_input_defaults` — verify it still passes.)

---

### Task 2: Define WireResponse struct

- **Description**: Define the flat wire format struct that's serialized into the MCP tool result's `content[0].text`.
- **Acceptance Criteria**:
  - [ ] WireResponse struct defined with all fields from the design
  - [ ] Derives `Debug, Serialize` with `rename_all = "camelCase"`
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: None
- **Code**:
  ```rust
  /// Flat wire response — what Claude sees in content[0].text.
  /// Merges echo-backs from ThoughtInput, trace metadata, and ThoughtResult fields.
  /// NOT ThoughtResult directly — field names differ (trajectory not mlTrajectory, etc).
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

      // From ThoughtResult (some renamed)
      pub warnings: Vec<String>,
      pub alerts: Vec<Alert>,
      pub confidence_reported: Option<f64>,
      pub confidence_calculated: Option<f64>,
      pub confidence_gap: Option<f64>,
      pub bias_detected: Option<String>,
      pub sycophancy: Option<String>,
      pub depth_overlap: Option<f64>,
      pub budget_used: u32,
      pub budget_max: u32,
      pub budget_category: String,
      pub trajectory: Option<f64>,          // ThoughtResult.ml_trajectory
      pub drift_detected: Option<bool>,     // ThoughtResult.ml_drift
      pub recap: Option<String>,

      // Completion-only
      pub adr: Option<String>,
      pub trust_score: Option<f64>,
      pub trust_reason: Option<String>,
  }
  ```
- **Test Cases** (file: `src/thought.rs`):
  - **`test_wire_response_serializes_camel_case`**: Create a WireResponse with known values. Serialize to JSON via `serde_json::to_value()`. Assert key names are camelCase: `value["traceId"]`, `value["thoughtNumber"]`, `value["nextThoughtNeeded"]`, `value["driftDetected"]`, `value["budgetCategory"]`.
  - **`test_wire_response_uses_trajectory_not_ml_trajectory`**: Serialize WireResponse. Assert `value.get("trajectory").is_some()` and `value.get("mlTrajectory").is_none()`.

---

### Task 3: Add Trace::new() constructor

- **Description**: Add a constructor to Trace that generates a UUID and sets created_at to current unix millis.
- **Acceptance Criteria**:
  - [ ] `Trace::new()` returns a Trace with UUID id, empty thoughts, current timestamp, closed=false
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: None
- **Code**:
  ```rust
  impl Trace {
      pub fn new() -> Self {
          Self {
              id: uuid::Uuid::new_v4().to_string(),
              thoughts: Vec::new(),
              created_at: std::time::SystemTime::now()
                  .duration_since(std::time::UNIX_EPOCH)
                  .unwrap()
                  .as_millis() as i64,
              closed: false,
          }
      }
  }
  ```
- **Test Cases** (file: `src/thought.rs`):
  - **`test_trace_new_generates_uuid`**: `let t = Trace::new();` Assert `t.id.len() == 36` (UUID format). Assert `t.thoughts.is_empty()`. Assert `!t.closed`. Assert `t.created_at > 0`.
  - **`test_trace_new_unique_ids`**: Create two traces. Assert `t1.id != t2.id`.

---

### Task 4: Add ThinkingServer::process_thought() no-op pipeline

- **Description**: Implement the per-thought processing method. Creates or looks up trace, appends thought record, builds WireResponse with defaults. This is the no-op pipeline — real analyzers/ML/DB wired in by later issues.
- **Acceptance Criteria**:
  - [ ] `process_thought()` creates new trace on `thought_number == 1` with no `trace_id`
  - [ ] `process_thought()` looks up existing trace when `trace_id` is provided
  - [ ] Returns error string if `trace_id` provided but not found
  - [ ] Appends ThoughtRecord to trace
  - [ ] Returns WireResponse with correct echo-backs and default computed fields
  - [ ] Marks trace closed when `next_thought_needed == false`
  - [ ] Method is `async` (takes write lock on traces RwLock)
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: Tasks 1, 2, 3
- **Code**:
  ```rust
  impl ThinkingServer {
      /// Process a single thought. No-op pipeline — returns defaults for all computed fields.
      /// Later issues (#3-#7) replace the no-ops with real logic.
      pub async fn process_thought(&self, input: ThoughtInput) -> Result<WireResponse, String> {
          let mut traces = self.traces.write().await;

          // Create or lookup trace
          let trace_id = if input.thought_number == 1 && input.trace_id.is_none() {
              let trace = Trace::new();
              let id = trace.id.clone();
              traces.insert(id.clone(), trace);
              id
          } else if let Some(ref id) = input.trace_id {
              if !traces.contains_key(id) {
                  return Err(format!("unknown trace: {}", id));
              }
              id.clone()
          } else {
              return Err("trace_id required for thought_number > 1".into());
          };

          let trace = traces.get_mut(&trace_id).unwrap();

          // Build default ThoughtResult (no-op — all analyzers/ML/warnings return defaults)
          let result = ThoughtResult::default();

          // Append record
          let now = std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .unwrap()
              .as_millis() as i64;

          trace.thoughts.push(ThoughtRecord {
              input: input.clone(),
              result: ThoughtResult::default(),
              created_at: now,
          });

          // Collect branch IDs from trace
          let branches: Vec<String> = trace.thoughts.iter()
              .filter_map(|t| t.input.branch_id.clone())
              .collect::<std::collections::HashSet<_>>()
              .into_iter()
              .collect();

          // Determine budget from config
          let (budget_used, budget_max, budget_category) = if let Some(ref mode) = input.thinking_mode {
              if let Some(mode_config) = self.config.modes.get(mode) {
                  let tier = &mode_config.budget;
                  if let Some(range) = self.config.budgets.get(tier) {
                      (input.thought_number, range[1], tier.clone())
                  } else {
                      (input.thought_number, 5, "standard".into())
                  }
              } else {
                  (input.thought_number, 5, "standard".into())
              }
          } else {
              (input.thought_number, 5, "standard".into())
          };

          // Build wire response
          let wire = WireResponse {
              trace_id: trace_id.clone(),
              thought_number: input.thought_number,
              total_thoughts: input.total_thoughts,
              next_thought_needed: input.next_thought_needed,
              branches,
              thought_history_length: trace.thoughts.len(),
              warnings: vec![],
              alerts: vec![],
              confidence_reported: input.confidence,
              confidence_calculated: None,
              confidence_gap: None,
              bias_detected: None,
              sycophancy: None,
              depth_overlap: None,
              budget_used,
              budget_max,
              budget_category,
              trajectory: None,
              drift_detected: None,
              recap: None,
              adr: None,
              trust_score: None,
              trust_reason: None,
          };

          // Close trace if done
          if !input.next_thought_needed {
              trace.closed = true;
          }

          Ok(wire)
      }
  }
  ```
- **Test Cases** (file: `src/thought.rs`):
  - **`test_process_thought_creates_trace`**: Build config, create ThinkingServer. Call `process_thought()` with `thought_number: 1, trace_id: None`. Assert `Ok(wire)` with `wire.thought_number == 1`, `wire.trace_id.len() == 36`, `wire.thought_history_length == 1`. `#[tokio::test]`
  - **`test_process_thought_second_thought`**: Call process_thought with thought 1, get trace_id. Call again with `thought_number: 2, trace_id: Some(trace_id)`. Assert `Ok(wire)` with `wire.thought_history_length == 2`.
  - **`test_process_thought_unknown_trace`**: Call process_thought with `thought_number: 2, trace_id: Some("nonexistent".into())`. Assert `Err` containing "unknown trace".
  - **`test_process_thought_closes_trace`**: Call process_thought with `next_thought_needed: false`. Verify trace is closed by checking `server.traces.read().await` — trace exists with `closed == true`.
  - **`test_process_thought_budget_from_config`**: Create config with `modes["architecture"].budget = "deep"` and `budgets["deep"] = [5, 8]`. Call process_thought with `thinking_mode: Some("architecture")`. Assert `wire.budget_max == 8`, `wire.budget_category == "deep"`.

  **Helper for tests**: Create a helper function that builds a minimal ThoughtInput:
  ```rust
  fn test_input(thought_number: u32, trace_id: Option<String>, next_needed: bool) -> ThoughtInput {
      ThoughtInput {
          trace_id,
          thought: "test thought".into(),
          thought_number,
          total_thoughts: 5,
          next_thought_needed: next_needed,
          thinking_mode: None,
          affected_components: vec![],
          confidence: None,
          evidence: vec![],
          estimated_impact: None,
          is_revision: false,
          revises_thought: None,
          branch_from_thought: None,
          branch_id: None,
          needs_more_thoughts: false,
      }
  }
  ```

---

### Task 5: Update existing tests for trace_id field

- **Description**: Update the existing `test_thought_input_deserialize` and `test_thought_input_defaults` tests to account for the new `trace_id` field. Verify backward compatibility.
- **Acceptance Criteria**:
  - [ ] All pre-existing 15 tests still pass
  - [ ] All new tests pass
  - [ ] `cargo test` passes with 0 failures
- **Files to Modify**:
  ```
  src/thought.rs  ← update existing test JSON strings if needed
  ```
- **Dependencies**: Tasks 1-4
- **Test verification**: Run `cargo test` — expect all original 15 tests + new tests to pass. The `trace_id` field has `#[serde(default)]` so existing test JSON (without `traceId`) should still deserialize fine.

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]` with `cargo test`, `#[tokio::test]` for async
- **Structure**: All tests inline in `src/thought.rs`
- **Coverage**: ~11 new tests + 15 existing = ~26 total
- **Run**: `cargo test --lib thought`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| `trace_id` breaks existing deserialization | Very Low | High | `#[serde(default)]` ensures backward compat | Existing tests catch it |
| WireResponse field names wrong | Low | High | Exact field list from design, camelCase serde | `test_wire_response_serializes_camel_case` |
| RwLock deadlock in process_thought | Low | High | Single write lock scope, no nested locks | `#[tokio::test]` tests exercise the lock |
| Budget lookup falls through | Low | Medium | Default fallback to "standard" / 5 | `test_process_thought_budget_from_config` |

## Success Criteria

- [ ] `trace_id: Option<String>` on ThoughtInput, deserializes from camelCase
- [ ] `WireResponse` serializes to flat camelCase JSON with correct field names
- [ ] `Trace::new()` generates unique UUIDs
- [ ] `process_thought()` creates traces, looks up traces, appends records, returns WireResponse
- [ ] Trace closed when `next_thought_needed == false`
- [ ] All ~26 tests pass (`cargo test`)
- [ ] No clippy warnings on new code

## Implementation Notes

- **Do NOT modify `src/config.rs` or `src/main.rs`** — only `src/thought.rs`
- **The `use crate::config::Config;` import already exists** in thought.rs
- **`uuid` crate is already in Cargo.toml** with `v4` feature
- **`std::collections::HashSet`** is in stdlib, no import needed beyond `use std::collections::HashSet;`
- **Existing `ThinkingServer::new()` is unchanged** — `process_thought()` is a new method alongside it
- **The `test_server_new` existing test** uses `Config::load("config/feldspar.toml", "config/principles.toml")` — reuse this pattern for new tests
