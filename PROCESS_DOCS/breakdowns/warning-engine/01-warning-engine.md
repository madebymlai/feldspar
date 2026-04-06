# Build Agent 1: Warning Engine

## Dependencies

None (single agent)

## Overview

- **Objective**: Implement the advisory warning system in `src/warnings.rs` — language regex checks, budget threshold warnings, and mode-specific field validation. Wire into `process_thought()` Phase 2.
- **Scope**:
  - Includes: `src/warnings.rs`, `src/config.rs` (add `resolve_budget()`), `src/thought.rs` (add `RecentProgress` to `TraceSnapshot`, call `generate_warnings()`, update inline budget resolution), `Cargo.toml` (add `regex`)
  - Excludes: No changes to `src/mcp.rs`, `src/llm.rs`, `src/main.rs`, `src/analyzers/`. The analyzer pipeline (issue #5) is separate.
- **Dependencies**:
  - Existing: `src/config.rs` (Config struct with modes, budgets, thresholds), `src/thought.rs` (ThoughtInput, ThoughtRecord, TraceSnapshot, WireResponse, process_thought Phase 1/2 pattern)
  - Crates: `regex` 1 (new), `serde`, `tracing` — all others already in Cargo.toml
- **Estimated Complexity**: Low-Medium — one new module with regex patterns, two small changes to existing modules

## Technical Approach

### Architecture
Three independent checkers called from a single entry point. All sync, no I/O, no async.

```
generate_warnings(input, recent_progress, config)
    ├── check_language(&input.thought) → Vec<String>
    ├── check_budget(input, recent_progress, config) → Vec<String>
    └── check_mode(input, config) → Vec<String>
    │
    └── dedup by label → Vec<String> → WireResponse.warnings
```

### Key Design Decisions
- `warnings: Vec<String>` stays separate from `alerts: Vec<Alert>`. Warning engine produces strings, analyzer pipeline (issue #5) produces structured Alerts. No merging.
- Regex compiled once via `std::sync::LazyLock` (std since Rust 1.80, no dep).
- `resolve_budget()` returns `Option<(u32, u32, String)>` — unknown modes fire `UNKNOWN-MODE` warning instead of silent fallback.
- Budget warnings graduate: OVER-ANALYSIS at 1.5x, OVERTHINKING at 2.0x (suppresses OVER-ANALYSIS).
- Within-label dedup per `generate_warnings()` call — "just do a quick hack" matches two patterns but fires one warning.

---

## Task Breakdown

### Task 1: Add `regex` crate to Cargo.toml

- **Description**: Add the `regex` dependency needed for language pattern matching.
- **Acceptance Criteria**:
  - [ ] `regex = "1"` added to `[dependencies]` in `Cargo.toml`
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  Cargo.toml
  ```
- **Dependencies**: None
- **Code**: Add after the `reqwest` line:
  ```toml
  regex = "1"
  ```

---

### Task 2: Add `resolve_budget()` to Config

- **Description**: Extract budget resolution logic into a reusable helper method on `Config`. Returns `Option` instead of hardcoded fallback.
- **Acceptance Criteria**:
  - [ ] `Config::resolve_budget(mode: Option<&str>) -> Option<(u32, u32, String)>` implemented
  - [ ] Returns `None` when mode is `None` or not found in config
  - [ ] Returns `Some((min, max, tier_name))` when mode and tier exist
  - [ ] `process_thought()` inline budget resolution replaced with call to `resolve_budget()`
  - [ ] `cargo check` passes
  - [ ] Existing tests still pass
- **Files to Modify**:
  ```
  src/config.rs    ← add resolve_budget() impl
  src/thought.rs   ← replace inline budget resolution
  ```
- **Dependencies**: None
- **Code — resolve_budget() (in config.rs)**:
  ```rust
  impl Config {
      /// Resolve thinking mode to budget (min, max, tier_name).
      /// Returns None if mode is None or not found in config.
      pub fn resolve_budget(&self, mode: Option<&str>) -> Option<(u32, u32, String)> {
          let mode_name = mode?;
          let mode_config = self.modes.get(mode_name)?;
          let tier = &mode_config.budget;
          let range = self.budgets.get(tier)?;
          Some((range[0], range[1], tier.clone()))
      }
  }
  ```
- **Code — Replace inline budget in thought.rs**: Find the block at approximately lines 225-239:
  ```rust
  // REPLACE THIS:
  let (budget_used, budget_max, budget_category) =
      if let Some(ref mode) = input.thinking_mode {
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

  // WITH THIS:
  let (budget_used, budget_max, budget_category) =
      match self.config.resolve_budget(input.thinking_mode.as_deref()) {
          Some((_, max, tier)) => (input.thought_number, max, tier),
          None => (input.thought_number, 5, "standard".into()),
      };
  ```
- **Test Cases** (file: `src/config.rs` `#[cfg(test)]`):
  - **`test_resolve_budget_architecture`**: `resolve_budget(Some("architecture"))` → `Some((5, 8, "deep"))`
  - **`test_resolve_budget_implementation`**: `resolve_budget(Some("implementation"))` → `Some((2, 3, "minimal"))`
  - **`test_resolve_budget_unknown_mode`**: `resolve_budget(Some("nonexistent"))` → `None`
  - **`test_resolve_budget_none_mode`**: `resolve_budget(None)` → `None`

---

### Task 3: Add RecentProgress to TraceSnapshot in thought.rs

- **Description**: Extract lightweight progress data from the last 3 branch-filtered records during Phase 1, add to TraceSnapshot for use by warning engine in Phase 2.
- **Acceptance Criteria**:
  - [ ] `RecentProgress` type alias defined: `pub type RecentProgress = Vec<(bool, Option<u32>)>`
  - [ ] `TraceSnapshot` has new field `recent_progress: RecentProgress`
  - [ ] Phase 1 extracts `(is_revision, branch_from_thought)` from last 3 branch-filtered records
  - [ ] `cargo check` passes
  - [ ] Existing tests updated to include `recent_progress` in TraceSnapshot construction
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: None
- **Code — Type alias** (near top of thought.rs, after `Timestamp` type):
  ```rust
  /// (is_revision, branch_from_thought) for last 3 records on current branch.
  /// Used by warning engine to check for recent progress.
  pub type RecentProgress = Vec<(bool, Option<u32>)>;
  ```
- **Code — TraceSnapshot field** (add to the existing struct):
  ```rust
  pub recent_progress: RecentProgress,
  ```
- **Code — Phase 1 extraction** (after the branch filtering for recap, inside the write lock block):
  ```rust
  let recent_progress: RecentProgress = trace.thoughts
      .iter()
      .filter(|t| t.input.branch_id == input.branch_id)
      .rev()
      .take(3)
      .map(|t| (t.input.is_revision, t.input.branch_from_thought))
      .collect();
  ```
  Add `recent_progress` to the `TraceSnapshot { ... }` construction.
- **Code — Update existing test helpers**: Any test that constructs a `TraceSnapshot` directly must include `recent_progress: vec![]`. Search for `TraceSnapshot {` in the test module and add the field.

---

### Task 4: Implement warning engine in src/warnings.rs

- **Description**: Replace the stub with the full implementation — three checkers, LazyLock regex compilation, within-label dedup.
- **Acceptance Criteria**:
  - [ ] `generate_warnings()` entry point implemented
  - [ ] `check_language()` matches 10 regex patterns (4 ANTI-QUICK-FIX, 6 DISMISSAL)
  - [ ] `check_budget()` fires OVER-ANALYSIS, OVERTHINKING, UNDERTHINKING, UNKNOWN-MODE
  - [ ] `check_mode()` fires NO-EVIDENCE, NO-COMPONENTS, NO-LATENCY, NO-CONFIDENCE
  - [ ] Budget warnings graduate (OVERTHINKING suppresses OVER-ANALYSIS)
  - [ ] Within-label dedup: multiple matches for same label produce one warning
  - [ ] All regex case-insensitive
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  src/warnings.rs    ← replace stub entirely
  ```
- **Dependencies**: Task 1 (regex crate), Task 2 (resolve_budget)
- **Code — Full implementation**:
  ```rust
  use crate::config::Config;
  use crate::thought::{RecentProgress, ThoughtInput};
  use regex::Regex;
  use std::collections::HashSet;
  use std::sync::LazyLock;

  struct WarningPattern {
      regex: Regex,
      label: &'static str,
      message: &'static str,
  }

  static LANGUAGE_PATTERNS: LazyLock<Vec<WarningPattern>> = LazyLock::new(|| {
      vec![
          // ANTI-QUICK-FIX patterns
          WarningPattern {
              regex: Regex::new(r"(?i)\b(just|simply)\s+(do|use|add|skip|ignore|throw|hack|slap)\b").unwrap(),
              label: "ANTI-QUICK-FIX",
              message: "Shortcut language detected — justify this approach or propose a proper solution.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bquick\s*(fix|solution|hack)\b").unwrap(),
              label: "ANTI-QUICK-FIX",
              message: "Shortcut language detected — justify this approach or propose a proper solution.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bgood\s+enough\b").unwrap(),
              label: "ANTI-QUICK-FIX",
              message: "Shortcut language detected — justify this approach or propose a proper solution.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bshould\s+be\s+fine\b").unwrap(),
              label: "ANTI-QUICK-FIX",
              message: "Shortcut language detected — justify this approach or propose a proper solution.",
          },
          // DISMISSAL patterns
          WarningPattern {
              regex: Regex::new(r"(?i)\bpre.?existing\s+(issue|problem|bug)").unwrap(),
              label: "DISMISSAL",
              message: "Dismissal language detected — address the issue or explain why it's out of scope.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bout\s+of\s+scope\b").unwrap(),
              label: "DISMISSAL",
              message: "Dismissal language detected — address the issue or explain why it's out of scope.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bnot\s+(my|our)\s+(problem|concern)\b").unwrap(),
              label: "DISMISSAL",
              message: "Dismissal language detected — address the issue or explain why it's out of scope.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\b(already|was)\s+broken\b").unwrap(),
              label: "DISMISSAL",
              message: "Dismissal language detected — address the issue or explain why it's out of scope.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bworked\s+before\b").unwrap(),
              label: "DISMISSAL",
              message: "Dismissal language detected — address the issue or explain why it's out of scope.",
          },
          WarningPattern {
              regex: Regex::new(r"(?i)\bknown\s+issue\b").unwrap(),
              label: "DISMISSAL",
              message: "Dismissal language detected — address the issue or explain why it's out of scope.",
          },
      ]
  });

  fn check_language(thought: &str) -> Vec<String> {
      LANGUAGE_PATTERNS
          .iter()
          .filter(|p| p.regex.is_match(thought))
          .map(|p| format!("WARNING [{}]: {}", p.label, p.message))
          .collect()
  }

  fn check_budget(
      input: &ThoughtInput,
      recent_progress: &RecentProgress,
      config: &Config,
  ) -> Vec<String> {
      let mut warnings = Vec::new();

      // Resolve budget for mode
      let budget = match config.resolve_budget(input.thinking_mode.as_deref()) {
          Some(b) => b,
          None => {
              // If mode was specified but not found, warn
              if let Some(ref mode) = input.thinking_mode {
                  warnings.push(format!(
                      "WARNING [UNKNOWN-MODE]: thinking_mode '{}' not found in config. No budget checks applied.",
                      mode
                  ));
              }
              return warnings;
          }
      };

      let (budget_min, budget_max, _tier) = budget;
      let thought_num = input.thought_number as f64;
      let total = input.total_thoughts as f64;

      let over_analysis_threshold = total * config.thresholds.over_analysis_multiplier;
      let overthinking_threshold = total * config.thresholds.overthinking_multiplier;

      // OVERTHINKING (2.0x, suppresses OVER-ANALYSIS)
      if thought_num > overthinking_threshold {
          let has_progress = recent_progress
              .iter()
              .any(|(is_revision, branch_from)| *is_revision || branch_from.is_some());
          if !has_progress {
              warnings.push(format!(
                  "WARNING [OVERTHINKING]: Past {}x your estimate with no new insights. Make a decision or branch.",
                  config.thresholds.overthinking_multiplier
              ));
          }
      }
      // OVER-ANALYSIS (1.5x, only if not already overthinking)
      else if thought_num > over_analysis_threshold {
          warnings.push(format!(
              "WARNING [OVER-ANALYSIS]: At thought {} of estimated {}. Conclude or justify continuing.",
              input.thought_number, input.total_thoughts
          ));
      }

      // UNDERTHINKING
      if !input.next_thought_needed && input.thought_number < budget_min {
          warnings.push(format!(
              "WARNING [UNDERTHINKING]: Wrapping up in {} thoughts when minimum for this mode is {}. This needs more depth.",
              input.thought_number, budget_min
          ));
      }

      warnings
  }

  fn check_mode(input: &ThoughtInput, config: &Config) -> Vec<String> {
      let mut warnings = Vec::new();

      let mode_name = match input.thinking_mode.as_deref() {
          Some(m) => m,
          None => return warnings,
      };

      let mode_config = match config.modes.get(mode_name) {
          Some(m) => m,
          None => return warnings, // UNKNOWN-MODE already handled by check_budget
      };

      for req in &mode_config.requires {
          match req.as_str() {
              "evidence" if input.evidence.is_empty() => {
                  warnings.push(format!(
                      "WARNING [NO-EVIDENCE]: {} mode requires citations — file paths, logs, stack traces.",
                      mode_name
                  ));
              }
              "components" if input.affected_components.is_empty() => {
                  warnings.push(format!(
                      "WARNING [NO-COMPONENTS]: {} mode requires naming affected components.",
                      mode_name
                  ));
              }
              "latency" => {
                  let missing = input.estimated_impact.as_ref()
                      .map_or(true, |imp| imp.latency.is_none());
                  if missing {
                      warnings.push(format!(
                          "WARNING [NO-LATENCY]: {} mode requires latency estimates.",
                          mode_name
                      ));
                  }
              }
              "confidence" if input.confidence.is_none() => {
                  warnings.push(format!(
                      "WARNING [NO-CONFIDENCE]: {} mode requires a confidence rating.",
                      mode_name
                  ));
              }
              _ => {}
          }
      }

      warnings
  }

  pub fn generate_warnings(
      input: &ThoughtInput,
      recent_progress: &RecentProgress,
      config: &Config,
  ) -> Vec<String> {
      let mut warnings = Vec::new();
      warnings.extend(check_language(&input.thought));
      warnings.extend(check_budget(input, recent_progress, config));
      warnings.extend(check_mode(input, config));

      // Dedup by label — keep first occurrence per [LABEL]
      let mut seen = HashSet::new();
      warnings.retain(|w| {
          let label = w.split(']').next().unwrap_or("");
          seen.insert(label.to_owned())
      });

      warnings
  }
  ```
- **Test Cases** (file: `src/warnings.rs` `#[cfg(test)]`):

  The tests need helper functions to build test inputs and configs. Here's the test helper pattern:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::config::*;
      use crate::thought::*;
      use std::collections::HashMap;

      fn test_config() -> Config {
          let mut modes = HashMap::new();
          modes.insert("architecture".into(), ModeConfig {
              requires: vec!["components".into()],
              budget: "deep".into(),
              watches: String::new(),
          });
          modes.insert("debugging".into(), ModeConfig {
              requires: vec!["evidence".into()],
              budget: "standard".into(),
              watches: String::new(),
          });
          modes.insert("implementation".into(), ModeConfig {
              requires: vec![],
              budget: "minimal".into(),
              watches: String::new(),
          });

          let mut budgets = HashMap::new();
          budgets.insert("minimal".into(), [2, 3]);
          budgets.insert("standard".into(), [3, 5]);
          budgets.insert("deep".into(), [5, 8]);

          Config {
              feldspar: FeldsparConfig {
                  db_path: String::new(),
                  model_path: String::new(),
                  recap_every: 3,
              },
              llm: LlmConfig {
                  base_url: None,
                  api_key_env: None,
                  model: String::new(),
              },
              thresholds: ThresholdsConfig {
                  confidence_gap: 25.0,
                  over_analysis_multiplier: 1.5,
                  overthinking_multiplier: 2.0,
              },
              budgets,
              pruning: PruningConfig {
                  no_outcome_days: 30,
                  low_quality_days: 15,
                  with_outcome_days: 90,
              },
              modes,
              components: ComponentsConfig { valid: vec![] },
              principles: vec![],
          }
      }

      fn test_input(thought: &str) -> ThoughtInput {
          ThoughtInput {
              trace_id: None,
              thought: thought.into(),
              thought_number: 1,
              total_thoughts: 5,
              next_thought_needed: true,
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
      // ... tests use test_input() with field overrides via struct update syntax
  }
  ```

  **Language tests:**
  - **`test_anti_quick_fix_just_do`**: `test_input("let's just do a quick hack")` → warnings contains "ANTI-QUICK-FIX"
  - **`test_anti_quick_fix_should_be_fine`**: `test_input("should be fine")` → contains "ANTI-QUICK-FIX"
  - **`test_dismissal_out_of_scope`**: `test_input("that's out of scope")` → contains "DISMISSAL"
  - **`test_dismissal_known_issue`**: `test_input("it's a known issue")` → contains "DISMISSAL"
  - **`test_clean_thought_no_warnings`**: `test_input("Let's analyze the trade-offs between PostgreSQL and Redis")` → `generate_warnings()` returns empty vec
  - **`test_case_insensitive`**: `test_input("JUST DO it")` → fires ANTI-QUICK-FIX
  - **`test_dedup_same_label`**: `test_input("let's just do a quick hack")` → only ONE "ANTI-QUICK-FIX" warning (not two)
  - **`test_false_positive_acknowledged`**: `test_input("we can simply use the existing trait implementation")` → fires ANTI-QUICK-FIX (advisory, expected)

  **Budget tests:**
  - **`test_over_analysis_fires`**: `thought_number=8, total_thoughts=5, thinking_mode=Some("debugging")` → contains "OVER-ANALYSIS"
  - **`test_over_analysis_within_limit`**: `thought_number=7, total_thoughts=5` → no "OVER-ANALYSIS" (7 < 7.5)
  - **`test_overthinking_fires`**: `thought_number=11, total_thoughts=5, thinking_mode=Some("debugging")`, `recent_progress=vec![(false, None), (false, None), (false, None)]` → contains "OVERTHINKING"
  - **`test_overthinking_suppressed_by_revision`**: same but `recent_progress=vec![(true, None), (false, None), (false, None)]` → no "OVERTHINKING"
  - **`test_overthinking_suppressed_by_new_branch`**: same but `recent_progress=vec![(false, Some(9)), (false, None), (false, None)]` → no "OVERTHINKING"
  - **`test_over_analysis_suppressed_when_overthinking`**: `thought_number=11, total_thoughts=5` → contains "OVERTHINKING", does NOT contain "OVER-ANALYSIS"
  - **`test_over_analysis_fires_alone_at_threshold`**: `thought_number=8, total_thoughts=5` → contains "OVER-ANALYSIS", does NOT contain "OVERTHINKING"
  - **`test_underthinking_fires`**: `next_thought_needed=false, thought_number=1, thinking_mode=Some("architecture")` → contains "UNDERTHINKING"
  - **`test_underthinking_ok_when_above_min`**: `next_thought_needed=false, thought_number=6, thinking_mode=Some("architecture")` → no "UNDERTHINKING"
  - **`test_unknown_mode_fires_warning`**: `thinking_mode=Some("nonexistent_mode")` → contains "UNKNOWN-MODE"
  - **`test_no_mode_no_budget_warnings`**: `thinking_mode=None` → no budget warnings at all
  - **`test_budget_threshold_float_boundary`**: `thought_number=8, total_thoughts=5, multiplier=1.5` → 8.0 > 7.5 fires. `thought_number=7` → 7.0 < 7.5 does not.

  **Mode tests:**
  - **`test_no_evidence_debugging`**: `thinking_mode=Some("debugging"), evidence=vec![]` → contains "NO-EVIDENCE"
  - **`test_no_components_architecture`**: `thinking_mode=Some("architecture"), affected_components=vec![]` → contains "NO-COMPONENTS"
  - **`test_no_warning_when_fields_present`**: `thinking_mode=Some("debugging"), evidence=vec!["file.rs".into()]` → no mode warnings
  - **`test_unknown_mode_no_mode_warnings`**: `thinking_mode=Some("nonexistent")` → no mode warnings (UNKNOWN-MODE handled by budget checker)
  - **`test_no_latency_custom_mode`**: Create config with mode requiring `"latency"`. Input with `estimated_impact=None` → "NO-LATENCY"
  - **`test_no_confidence_custom_mode`**: Create config with mode requiring `"confidence"`. Input with `confidence=None` → "NO-CONFIDENCE"

  **Integration test:**
  - **`test_generate_warnings_merges_all`**: thought = "let's just do a quick hack", `thought_number=8, total_thoughts=5, thinking_mode=Some("debugging"), evidence=vec![]` → contains "ANTI-QUICK-FIX" AND "OVER-ANALYSIS" AND "NO-EVIDENCE"

---

### Task 5: Wire generate_warnings() into process_thought()

- **Description**: Call `generate_warnings()` in Phase 2 and populate `WireResponse.warnings`.
- **Acceptance Criteria**:
  - [ ] `generate_warnings()` called in Phase 2 with input, recent_progress from snapshot, and config
  - [ ] Result assigned to `wire.warnings`
  - [ ] `cargo check` passes
  - [ ] Existing tests still pass
- **Files to Modify**:
  ```
  src/thought.rs
  ```
- **Dependencies**: Tasks 3, 4
- **Code**: In `process_thought()` Phase 2, before building the WireResponse, add:
  ```rust
  use crate::warnings::generate_warnings;

  // In Phase 2, after recap and before WireResponse construction:
  let warnings = generate_warnings(&input, &snapshot.recent_progress, &self.config);
  ```
  Then in the WireResponse construction, replace `warnings: vec![]` with `warnings`.

---

### Task 6: Run all tests and verify

- **Description**: Run `cargo test` and verify all existing + new tests pass.
- **Acceptance Criteria**:
  - [ ] All existing tests pass (~75 from issues #1-3)
  - [ ] New config tests pass (resolve_budget — 4 tests)
  - [ ] New warning tests pass (~25 tests)
  - [ ] `cargo check` passes with no errors
  - [ ] Only expected dead_code warnings
- **Dependencies**: Tasks 1-5
- **Run**: `cargo test`

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]`, `cargo test`
- **Structure**: Tests inline in `src/warnings.rs` (main test suite) and `src/config.rs` (resolve_budget tests)
- **Coverage**: ~4 config tests + ~25 warning tests + all ~75 existing tests
- **Run**: `cargo test`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| Regex patterns have unexpected false positives | Medium | Low | Warnings are advisory — false positives tolerable. Added `\b` word boundaries. | test_false_positive_acknowledged |
| LazyLock first-call stall blocks async runtime | Low | Low | ~100us one-time cost, acceptable | Manual timing if concerned |
| Budget graduation logic produces no warnings when both thresholds crossed | Low | Medium | else-if chain: OVERTHINKING check first, OVER-ANALYSIS only if not overthinking | test_over_analysis_suppressed_when_overthinking |
| TraceSnapshot changes break existing tests | Medium | Low | Add `recent_progress: vec![]` to all existing TraceSnapshot constructions | cargo test |
| resolve_budget() Option breaks process_thought() callers | Low | Medium | process_thought() uses match with fallback for None case | test_resolve_budget_none_mode + existing thought tests |

## Success Criteria

- [ ] `generate_warnings()` produces correct warnings for all 10 language patterns
- [ ] Budget warnings fire at correct thresholds (1.5x, 2.0x) and graduate correctly
- [ ] Unknown modes produce `UNKNOWN-MODE` warning instead of silent fallback
- [ ] Mode validation fires for all 4 `requires` values
- [ ] Within-label dedup works (one warning per label per call)
- [ ] All ~100 tests pass (75 existing + ~29 new)
- [ ] `cargo check` passes

## Implementation Notes

- **Do NOT modify `src/mcp.rs`, `src/llm.rs`, `src/main.rs`, `src/analyzers/`** — out of scope
- **Existing `test_config()` helpers** in `config.rs` and `mcp.rs` tests build Config programmatically. They will need the same fields. Check that `resolve_budget()` doesn't break them — it's an `impl` method on Config, not a struct change, so it shouldn't.
- **`WireResponse` already has `warnings: Vec<String>`** — no schema change needed. Just populate it.
- **`ThoughtInput` already has all required fields** — `thinking_mode`, `evidence`, `affected_components`, `estimated_impact`, `confidence`, `is_revision`, `branch_from_thought`, `next_thought_needed`. No schema changes.
- **The `warnings` field in `WireResponse` does NOT have `skip_serializing_if`** — it always appears in the response (even empty). This is correct — Claude should see `"warnings": []` to know the field exists.
- **Float comparison**: Cast `thought_number` and `total_thoughts` to `f64` before multiplying: `input.thought_number as f64 > input.total_thoughts as f64 * config.thresholds.over_analysis_multiplier`.
