# Build Agent 2: Thought Processor — Recap, ADR, Eviction

## Dependencies

**01-config-llm-client.md must complete first.** This agent imports `LlmClient` from `src/llm.rs` and uses `LlmConfig` from `src/config.rs`.

## Overview

- **Objective**: Wire real recap generation (LLM), ADR skeleton (template), and trace eviction (remove + Arc + spawn) into `process_thought()`. Refactor to two-phase design so write lock is never held across async LLM calls.
- **Scope**:
  - Includes: `src/thought.rs` (major refactor of process_thought), `src/main.rs` (wire LlmClient)
  - Excludes: No changes to `src/config.rs`, `src/llm.rs`, `src/mcp.rs` (Agent 1 handles config/llm)
- **Dependencies**:
  - Agent 1 output: `LlmConfig`, `LlmClient` from `src/llm.rs` and `src/config.rs`
  - Existing: `src/thought.rs` (process_thought from issue #2), `src/main.rs` (server startup)
- **Estimated Complexity**: Medium — two-phase refactor, async LLM integration, ADR template, eviction

## Technical Approach

### Two-Phase process_thought

The current `process_thought()` holds `traces.write().await` for the entire method. Adding an async LLM call (recap) inside this lock would block all concurrent traces. Refactor into:

**Phase 1** (write lock held — microseconds only):
- Create/lookup trace, append record
- Compute budget
- If recap due: clone branch-filtered thought texts as String (for LLM prompt)
- If completing: `traces.remove(&trace_id)` → owned Trace for eviction
- **Drop write lock explicitly**

**Phase 2** (no lock):
- If recap due: call `llm_client.chat_json()` (async, ~300ms, best-effort)
- If completing: generate ADR from trace data, Arc-wrap trace, spawn background tasks
- Build WireResponse

### Recap
- Every `config.feldspar.recap_every` thoughts (default 3)
- Branch filtering: if `branch_id.is_none()` → only main-line thoughts; if `Some(b)` → only that branch's thoughts
- Prompt: system asks for JSON `{"recap": "..."}`, user sends numbered thoughts
- Failure: `tracing::warn!`, recap stays None

### ADR
- Template from trace data, not LLM
- `BTreeSet` for deterministic component/mode ordering
- Decision = last main-line thought (not last chronological)
- Branches explored = first thought text from each branch (truncated to 100 chars)

### Eviction
- `traces.remove()` → `Arc::new(trace)` → spawn background tasks
- Background tasks are still no-ops (issues #5-#7 wire real ones)
- `if let Some(trace)` — no unwrap

### ThinkingServer changes
- Add `llm: Option<LlmClient>` field
- Update `new()` to accept `Option<LlmClient>`

---

## Task Breakdown

### Task 1: Add LlmClient to ThinkingServer and update main.rs

- **Description**: Add `llm` field to ThinkingServer, update constructor, wire in main.rs.
- **Acceptance Criteria**:
  - [ ] `ThinkingServer` has `pub llm: Option<LlmClient>` field
  - [ ] `ThinkingServer::new()` accepts `config: Arc<Config>, llm: Option<LlmClient>`
  - [ ] `main.rs` creates `LlmClient::new(&config.llm)` and passes to ThinkingServer
  - [ ] `main.rs` has `mod llm;` declaration (if not added by Agent 1)
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  src/thought.rs  ← add llm field + update new()
  src/main.rs     ← create LlmClient, pass to ThinkingServer, add mod llm
  ```
- **Dependencies**: Agent 1 complete
- **Code — ThinkingServer changes**:
  ```rust
  use crate::llm::LlmClient;

  pub struct ThinkingServer {
      pub traces: RwLock<HashMap<String, Trace>>,
      pub config: Arc<Config>,
      pub db: Option<DbPool>,
      pub ml: Option<MlModel>,
      pub llm: Option<LlmClient>,
  }

  impl ThinkingServer {
      pub fn new(config: Arc<Config>, llm: Option<LlmClient>) -> Self {
          Self {
              traces: RwLock::new(HashMap::new()),
              config,
              db: None,
              ml: None,
              llm,
          }
      }
  }
  ```
- **Code — main.rs changes** (in `run_server()`):
  ```rust
  use crate::llm::LlmClient;

  async fn run_server(port: u16) {
      let config = config::Config::load("config/feldspar.toml", "config/principles.toml");
      let llm = LlmClient::new(&config.llm);
      let server = thought::ThinkingServer::new(config, llm);
      // ... rest unchanged
  }
  ```
- **Note**: Update ALL existing `ThinkingServer::new(config)` calls to `ThinkingServer::new(config, None)` in tests (thought.rs tests, mcp.rs tests).
- **Test Cases**: No new tests — existing tests updated to compile with new signature.

---

### Task 2: Refactor process_thought to two-phase design

- **Description**: Split process_thought into Phase 1 (lock held, extract data) and Phase 2 (no lock, async work). This is the core architectural change.
- **Acceptance Criteria**:
  - [ ] Write lock dropped before any async LLM call
  - [ ] Recap data (branch-filtered thought texts) extracted in Phase 1
  - [ ] ADR data (components, modes, decision, branches) extracted in Phase 1
  - [ ] Eviction (`traces.remove()`) happens in Phase 1
  - [ ] WireResponse built in Phase 2
  - [ ] All existing process_thought tests still pass
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: Task 1
- **Code — Two-phase skeleton**:
  ```rust
  pub async fn process_thought(&self, input: ThoughtInput) -> Result<WireResponse, String> {
      // === PHASE 1: Write lock held (microseconds) ===
      let mut recap_text: Option<String> = None;
      let mut removed_trace: Option<Trace> = None;
      let mut trace_data: Option<TraceSnapshot> = None; // extracted data for wire response

      {
          let mut traces = self.traces.write().await;

          // Create or lookup trace (existing logic)
          let trace_id = /* ... same as before ... */;
          let trace = traces.get_mut(&trace_id).unwrap();

          // Append record (existing logic)
          trace.thoughts.push(/* ... */);

          // Extract data needed for Phase 2
          let branches: Vec<String> = /* ... BTreeSet for deterministic order ... */;
          let thought_history_length = trace.thoughts.len();
          let (budget_used, budget_max, budget_category) = /* ... existing budget logic ... */;

          // If recap due: extract branch-filtered thought texts
          if input.thought_number > 1
              && input.thought_number % self.config.feldspar.recap_every == 0
          {
              let filtered: Vec<&ThoughtRecord> = trace.thoughts.iter()
                  .filter(|t| t.input.branch_id == input.branch_id)
                  .collect();
              let formatted = filtered.iter().enumerate()
                  .map(|(i, t)| format!("Thought {}: {}", i + 1, t.input.thought))
                  .collect::<Vec<_>>()
                  .join("\n\n");
              recap_text = Some(formatted);
          }

          // If completing: extract ADR data + remove trace
          if !input.next_thought_needed {
              // Extract ADR data before removing
              // (clone what generate_adr needs)
              removed_trace = traces.remove(&trace_id);
          }

          // Store snapshot for wire response building
          trace_data = Some(TraceSnapshot {
              trace_id,
              branches,
              thought_history_length,
              budget_used,
              budget_max,
              budget_category,
          });

          // Write lock drops here (end of block)
      }

      // === PHASE 2: No lock held ===
      let data = trace_data.unwrap();

      // Recap (async LLM call — safe, no lock held)
      let recap = if let Some(text) = recap_text {
          self.generate_recap(&text).await
      } else {
          None
      };

      // ADR + eviction
      let adr = if let Some(ref trace) = removed_trace {
          Some(generate_adr(trace))
      } else {
          None
      };

      // Spawn background tasks for evicted trace
      if let Some(trace) = removed_trace {
          let trace = std::sync::Arc::new(trace);
          let t1 = trace.clone();
          tokio::spawn(async move { let _ = &t1; /* db_flush - no-op */ });
          let t2 = trace.clone();
          tokio::spawn(async move { let _ = &t2; /* trace_review - no-op */ });
          tokio::spawn(async move { let _ = &trace; /* ml_train - no-op */ });
      }

      // Build wire response
      Ok(WireResponse {
          trace_id: data.trace_id,
          thought_number: input.thought_number,
          total_thoughts: input.total_thoughts,
          next_thought_needed: input.next_thought_needed,
          branches: data.branches,
          thought_history_length: data.thought_history_length,
          warnings: vec![],
          alerts: vec![],
          confidence_reported: input.confidence,
          confidence_calculated: None,
          confidence_gap: None,
          bias_detected: None,
          sycophancy: None,
          depth_overlap: None,
          budget_used: data.budget_used,
          budget_max: data.budget_max,
          budget_category: data.budget_category,
          trajectory: None,
          drift_detected: None,
          recap,
          adr,
          trust_score: None,
          trust_reason: None,
      })
  }
  ```
- **Helper struct** (add to thought.rs, private):
  ```rust
  /// Extracted data from Phase 1 for building WireResponse in Phase 2
  struct TraceSnapshot {
      trace_id: String,
      branches: Vec<String>,
      thought_history_length: usize,
      budget_used: u32,
      budget_max: u32,
      budget_category: String,
  }
  ```
- **Test Cases** (file: `src/thought.rs`):
  - **`test_concurrent_thoughts_during_recap`**: Create two ThinkingServers sharing same traces (or use one server). Start thought 1 on trace A. Start thought 1 on trace B. Process thought 3 on trace A (triggers recap — but llm is None so it's instant). Verify trace B operations aren't blocked. `#[tokio::test]`
  - All existing `test_process_thought_*` tests must still pass.

---

### Task 3: Implement generate_recap()

- **Description**: Async method on ThinkingServer that calls LlmClient for recap.
- **Acceptance Criteria**:
  - [ ] Calls `self.llm.chat_json()` with recap prompt
  - [ ] Returns `Option<String>` — the recap text
  - [ ] Returns None if `self.llm` is None
  - [ ] Returns None on LLM failure (best-effort)
  - [ ] Recap prompt uses JSON format: `{"recap": "..."}`
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: Task 2
- **Code**:
  ```rust
  const RECAP_SYSTEM_PROMPT: &str = "You summarize thinking traces. Given numbered thoughts, \
      produce a 1-2 sentence recap capturing the key progression and current conclusion. \
      Respond with ONLY a JSON object: {\"recap\": \"<your summary>\"}";

  impl ThinkingServer {
      async fn generate_recap(&self, thoughts_text: &str) -> Option<String> {
          let llm = self.llm.as_ref()?;
          let result = llm.chat_json(RECAP_SYSTEM_PROMPT, thoughts_text, 200).await?;
          result["recap"].as_str().map(|s| s.to_owned())
      }
  }
  ```
- **Test Cases** (file: `src/thought.rs`):
  - **`test_recap_skipped_without_llm`**: ThinkingServer with `llm: None`. Process 3 thoughts. Verify `wire.recap.is_none()` on thought 3.
  - **`test_recap_on_third_thought`**: ThinkingServer with `llm: None` (recap will be None but the *attempt* is what matters). Process thoughts 1, 2, 3 with `recap_every = 3`. Verify thought 3 attempted recap (recap is None because no LLM, but no panic).
  - **`test_recap_branch_filtering`**: Create trace with main-line thoughts 1-2 and branch "alt" thought 3. Process thought 3 on main line. Verify the recap text (extracted in Phase 1) includes only main-line thoughts, not branch thoughts. This tests the filtering logic, not the LLM call.

---

### Task 4: Implement generate_adr()

- **Description**: Sync function that builds ADR skeleton from trace data using BTreeSet for deterministic ordering.
- **Acceptance Criteria**:
  - [ ] ADR contains date (YYYY-MM-DD), components, modes, decision, branches explored
  - [ ] Components and modes sorted via BTreeSet
  - [ ] Decision = last main-line thought (branch_id.is_none()), not last chronological
  - [ ] Branches explored = first thought text from each branch (truncated to 100 chars)
  - [ ] "none" when no components/modes/branches
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: Task 2
- **Code**:
  ```rust
  use std::collections::BTreeSet;

  fn generate_adr(trace: &Trace) -> String {
      let now = chrono::Utc::now(); // OR use manual date formatting without chrono
      // Since we don't have chrono, use a simple approach:
      let date = {
          let secs = std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .unwrap()
              .as_secs();
          // Simple UTC date: days since epoch
          let days = secs / 86400;
          let (y, m, d) = days_to_ymd(days); // helper or just use timestamp string
          format!("{}-{:02}-{:02}", y, m, d)
      };
      // Alternatively, just use the trace.created_at timestamp formatted as date.
      // Simplest: store as "2026-04-06" style string.
      // For MVP: use a simple hardcoded approach or the `time` crate if available.
      // SIMPLEST: Just format the unix millis into a date string manually.

      let components: Vec<String> = trace.thoughts.iter()
          .flat_map(|t| t.input.affected_components.iter().cloned())
          .collect::<BTreeSet<_>>().into_iter().collect();

      let modes: Vec<String> = trace.thoughts.iter()
          .filter_map(|t| t.input.thinking_mode.clone())
          .collect::<BTreeSet<_>>().into_iter().collect();

      // Decision = last main-line thought
      let decision = trace.thoughts.iter()
          .filter(|t| t.input.branch_id.is_none())
          .last()
          .map(|t| t.input.thought.as_str())
          .unwrap_or("No conclusion");

      // Branches explored = first thought text from each branch
      let mut branch_descriptions: Vec<String> = Vec::new();
      let mut seen_branches = BTreeSet::new();
      for t in &trace.thoughts {
          if let Some(ref bid) = t.input.branch_id {
              if seen_branches.insert(bid.clone()) {
                  let text = if t.input.thought.len() > 100 {
                      format!("{}: {}...", bid, &t.input.thought[..100])
                  } else {
                      format!("{}: {}", bid, t.input.thought)
                  };
                  branch_descriptions.push(text);
              }
          }
      }

      format!(
          "## ADR\n**Date**: {}\n**Components**: {}\n**Mode**: {}\n**Decision**: {}\n**Branches explored**: {}",
          date,
          if components.is_empty() { "none".into() } else { components.join(", ") },
          if modes.is_empty() { "none".into() } else { modes.join(", ") },
          decision,
          if branch_descriptions.is_empty() { "none".into() } else { branch_descriptions.join("; ") },
      )
  }
  ```
  **Date formatting note**: We don't have `chrono` in dependencies. For the date in ADR, convert `trace.created_at` (i64 unix millis) to YYYY-MM-DD. Simplest approach: add a small helper function, or just use the raw timestamp. The exact format isn't critical for an ADR skeleton — even `"timestamp: 1775504065000"` would work for MVP, but YYYY-MM-DD is cleaner. A simple days-since-epoch calculation works.
- **Test Cases** (file: `src/thought.rs`):
  - **`test_generate_adr_basic`**: Create trace with 2 main-line thoughts, components=["auth"], mode="architecture". Call `generate_adr()`. Assert contains "auth", "architecture", second thought text as decision.
  - **`test_generate_adr_decision_from_mainline`**: Create trace with main thought 1, branch thought 2 ("alt-1"), main thought 3. Assert decision is thought 3 text (not thought 2).
  - **`test_generate_adr_with_branches`**: Create trace with branch "alt-1" starting at thought 2. Assert "Branches explored" contains "alt-1: <thought text>".
  - **`test_generate_adr_no_components`**: Create trace with no components. Assert "Components: none".
  - **`test_generate_adr_deterministic`**: Call `generate_adr()` twice on same trace. Assert identical output.

---

### Task 5: Wire everything together and run all tests

- **Description**: Ensure all changes compile together, update all test helpers, run full test suite.
- **Acceptance Criteria**:
  - [ ] `ThinkingServer::new()` signature updated everywhere (thought.rs tests, mcp.rs tests)
  - [ ] `cargo test` passes with all existing + new tests
  - [ ] Recap works (returns None without LLM, doesn't panic)
  - [ ] ADR generated on completion
  - [ ] Trace evicted from HashMap on completion
  - [ ] No clippy warnings on new code
- **Files to Modify**:
  ```
  src/thought.rs  ← update test helpers
  src/mcp.rs      ← update McpState::new() / test helpers to pass llm=None
  ```
- **Dependencies**: Tasks 1-4
- **Test Cases**:
  - **`test_process_thought_adr_on_completion`**: Process thought 1, then thought 2 with `next_thought_needed: false`. Assert `wire.adr.is_some()`. Assert ADR contains date and decision text.
  - **`test_eviction_removes_trace`**: Process thought 1, then thought 2 with `next_thought_needed: false`. Check `server.traces.read().await.is_empty()`.
  - **`test_eviction_map_empty_after_close`**: Same as above — verify HashMap has 0 entries.
  - All existing 53 tests must still pass.

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]`, `#[tokio::test]`, `cargo test`
- **Structure**: Tests inline in `src/thought.rs`
- **Coverage**: ~12 new tests + all existing tests
- **Run**: `cargo test`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| Two-phase refactor breaks existing tests | High | Medium | Run tests after each task, fix incrementally | cargo test |
| ThinkingServer::new() signature change breaks mcp.rs | Guaranteed | Low | Update mcp.rs test helpers in Task 5 | Compile error |
| Date formatting without chrono | Low | Low | Simple unix-to-date helper, or use timestamp | test_generate_adr_basic |
| Eviction races in Phase 1 | Low | Medium | `if let Some(trace)` not unwrap | Existing tests |

## Success Criteria

- [ ] `process_thought()` never holds write lock across async calls
- [ ] Recap generated every N thoughts (None without LLM, string with LLM)
- [ ] ADR generated on completion with deterministic ordering
- [ ] Trace removed from HashMap on completion
- [ ] Background tasks spawned with `Arc<Trace>`
- [ ] All tests pass (existing + new)

## Implementation Notes

- **`use std::collections::BTreeSet;`** — add this import alongside existing `HashSet`
- **Replace `HashSet` with `BTreeSet`** in the branches collection too (for deterministic WireResponse ordering)
- **`ThinkingServer::new()` signature change** affects: `src/thought.rs` tests (`test_server()` helper), `src/mcp.rs` tests (`test_config()` or wherever McpState is built). Update ALL call sites.
- **`src/mcp.rs` McpState::new()** creates a ThinkingServer — it will need `llm: None` passed. Check how McpState builds the server and update.
- **Don't modify `src/config.rs` or `src/llm.rs`** — Agent 1 owns those.
- **The `removed_trace` pattern**: In Phase 1, if completing, `traces.remove()` takes the trace out of the map. The wire response is built from `TraceSnapshot` (extracted before removal) + ADR + recap. The removed trace goes to background tasks via Arc. This means the trace data is split: snapshot for response, full trace for background tasks.
