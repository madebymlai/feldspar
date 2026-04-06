# Build Agent 1: Pipeline Core + Observers

## Dependencies

None (parallel) — Agent 2 depends on this.

## Overview

- **Objective**: Implement the analyzer pipeline infrastructure (traits, types, `run_pipeline()`) and all 3 observers (depth, budget, bias). Agent 2 adds evaluators and wires into `process_thought()`.
- **Scope**:
  - Includes: `src/analyzers/mod.rs`, `src/analyzers/depth.rs`, `src/analyzers/budget.rs`, `src/analyzers/bias.rs`, `Cargo.toml`
  - Excludes: `src/analyzers/confidence.rs`, `src/analyzers/sycophancy.rs`, `src/thought.rs` (Agent 2)
- **Dependencies**:
  - Existing: `src/thought.rs` (ThoughtInput, ThoughtRecord, Alert, Severity types), `src/config.rs` (Config, resolve_budget)
  - Crates: `rayon` 1.10 (new), `strsim` 0.11 (existing)
- **Estimated Complexity**: Medium — 4 files, traits + 3 observer implementations with heuristic logic

## Technical Approach

### Architecture
Two-phase pipeline: observers run in parallel via rayon, produce `Observation` variants, merged into `Observations` struct. Evaluators (Agent 2) consume `Observations`. Each observer/evaluator wrapped in `catch_unwind` for fault isolation.

### Similarity Function
All similarity measurements use `strsim::normalized_levenshtein`. This is specified throughout — do not use `jaro_winkler` or other strsim functions. Thresholds:
- `> 0.7` = high similarity (rephrasing / same topic)
- `0.3 - 0.7` = moderate (building on / related)
- `< 0.3` = low (topic switch)

---

## Task Breakdown

### Task 1: Add `rayon` to Cargo.toml

- **Description**: Add rayon dependency for parallel iteration.
- **Acceptance Criteria**:
  - [ ] `rayon = "1.10"` added to `[dependencies]`
  - [ ] `cargo check` passes
- **Files to Modify**: `Cargo.toml`
- **Dependencies**: None
- **Code**: Add after the `regex` line:
  ```toml
  rayon = "1.10"
  ```

---

### Task 2: Implement pipeline core in mod.rs

- **Description**: Replace the stub with traits, types, and `run_pipeline()`. This is the foundation for all analyzers.
- **Acceptance Criteria**:
  - [ ] `Observer` trait with `Send + Sync` bounds
  - [ ] `Evaluator` trait with `Send + Sync` bounds
  - [ ] `Observation` enum with `Depth`, `Budget`, `Bias` variants
  - [ ] `Observations` struct with `pending_alerts`, `drain_alerts()`, `merge()`
  - [ ] `EvalOutput` enum with `Confidence` and `Sycophancy` variants
  - [ ] `PipelineResult` struct
  - [ ] `run_pipeline()` with rayon `par_iter`, named tuple dispatch, `catch_unwind`
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/mod.rs`
- **Dependencies**: Task 1

- **Code — Traits**:
  ```rust
  use crate::config::Config;
  use crate::thought::{Alert, Severity, ThoughtInput, ThoughtRecord};
  use rayon::prelude::*;
  use std::panic::{catch_unwind, AssertUnwindSafe};

  pub trait Observer: Send + Sync {
      fn observe(&self, input: &ThoughtInput, records: &[ThoughtRecord], config: &Config) -> Observation;
  }

  pub trait Evaluator: Send + Sync {
      fn evaluate(&self, input: &ThoughtInput, records: &[ThoughtRecord],
                  observations: &Observations, config: &Config) -> EvalOutput;
  }
  ```

- **Code — Observation enum**:
  ```rust
  pub enum Observation {
      Depth {
          prev_overlap: f64,
          initial_overlap: Option<f64>,
          contradictions: Vec<(u32, u32)>,
          shallow: bool,
          alerts: Vec<Alert>,
      },
      Budget {
          used: u32,
          max: u32,
          category: String,
      },
      Bias {
          detected: Option<String>,
      },
  }
  ```

- **Code — Observations struct**:
  ```rust
  #[derive(Default)]
  pub struct Observations {
      pub prev_overlap: Option<f64>,
      pub initial_overlap: Option<f64>,
      pub contradictions: Vec<(u32, u32)>,
      pub shallow: bool,
      pub budget_used: u32,
      pub budget_max: u32,
      pub budget_category: String,
      pub bias_detected: Option<String>,
      pub pending_alerts: Vec<Alert>,
  }

  impl Observations {
      pub fn merge(&mut self, obs: Observation) {
          match obs {
              Observation::Depth { prev_overlap, initial_overlap, contradictions, shallow, alerts } => {
                  self.prev_overlap = Some(prev_overlap);
                  self.initial_overlap = initial_overlap;
                  self.contradictions = contradictions;
                  self.shallow = shallow;
                  self.pending_alerts.extend(alerts);
              }
              Observation::Budget { used, max, category } => {
                  self.budget_used = used;
                  self.budget_max = max;
                  self.budget_category = category;
              }
              Observation::Bias { detected } => {
                  self.bias_detected = detected;
              }
          }
      }

      pub fn drain_alerts(&mut self) -> Vec<Alert> {
          std::mem::take(&mut self.pending_alerts)
      }
  }
  ```

- **Code — EvalOutput enum**:
  ```rust
  pub enum EvalOutput {
      Confidence {
          calculated: f64,
          alert: Option<Alert>,
      },
      Sycophancy {
          pattern: Option<String>,
          alert: Option<Alert>,
      },
  }
  ```

- **Code — PipelineResult**:
  ```rust
  pub struct PipelineResult {
      pub alerts: Vec<Alert>,
      pub observations: Observations,
      pub confidence_calculated: Option<f64>,
      pub sycophancy_pattern: Option<String>,
      pub panic_warnings: Vec<String>,
  }
  ```

- **Code — run_pipeline()**: 
  ```rust
  pub fn run_pipeline(
      input: &ThoughtInput,
      records: &[ThoughtRecord],
      config: &Config,
  ) -> PipelineResult {
      let mut panic_warnings = Vec::new();

      // Phase 1: Observers (named tuples)
      let observers: Vec<(&str, Box<dyn Observer>)> = vec![
          ("depth", Box::new(depth::DepthObserver)),
          ("budget", Box::new(budget::BudgetObserver)),
          ("bias", Box::new(bias::BiasObserver)),
      ];

      let observer_results: Vec<(&str, Result<Observation, _>)> = observers
          .par_iter()
          .map(|(name, obs)| {
              let result = catch_unwind(AssertUnwindSafe(|| {
                  obs.observe(input, records, config)
              }));
              (*name, result)
          })
          .collect();

      let mut observations = Observations::default();
      for (name, result) in observer_results {
          match result {
              Ok(obs) => observations.merge(obs),
              Err(_) => {
                  panic_warnings.push(format!("WARNING [ANALYZER-PANIC]: {} observer panicked", name));
              }
          }
      }

      // Phase 2: Evaluators (Agent 2 will add these — for now return empty)
      let evaluators: Vec<(&str, Box<dyn Evaluator>)> = vec![
          // Agent 2 adds: ("confidence", Box::new(confidence::ConfidenceEvaluator)),
          // Agent 2 adds: ("sycophancy", Box::new(sycophancy::SycophancyEvaluator)),
      ];

      let eval_results: Vec<(&str, Result<EvalOutput, _>)> = evaluators
          .par_iter()
          .map(|(name, eval)| {
              let result = catch_unwind(AssertUnwindSafe(|| {
                  eval.evaluate(input, records, &observations, config)
              }));
              (*name, result)
          })
          .collect();

      let mut alerts = Vec::new();
      let mut confidence_calculated = None;
      let mut sycophancy_pattern = None;

      for (name, result) in eval_results {
          match result {
              Ok(EvalOutput::Confidence { calculated, alert }) => {
                  confidence_calculated = Some(calculated);
                  if let Some(a) = alert { alerts.push(a); }
              }
              Ok(EvalOutput::Sycophancy { pattern, alert }) => {
                  sycophancy_pattern = pattern;
                  if let Some(a) = alert { alerts.push(a); }
              }
              Err(_) => {
                  panic_warnings.push(format!("WARNING [ANALYZER-PANIC]: {} evaluator panicked", name));
              }
          }
      }

      alerts.extend(observations.drain_alerts());

      PipelineResult { alerts, observations, confidence_calculated, sycophancy_pattern, panic_warnings }
  }
  ```

  **Note**: The evaluator vec is empty in Agent 1. Agent 2 will uncomment the evaluator lines and add the actual evaluator structs. This compiles and runs — it just produces no evaluator output.

- **Test Cases** (file: `src/analyzers/mod.rs` `#[cfg(test)]`):

  Helper function:
  ```rust
  #[cfg(test)]
  pub(crate) fn test_input(thought: &str) -> ThoughtInput {
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

  #[cfg(test)]
  pub(crate) fn test_record(thought: &str, number: u32) -> ThoughtRecord {
      ThoughtRecord {
          input: ThoughtInput {
              thought: thought.into(),
              thought_number: number,
              ..test_input("")
          },
          result: ThoughtResult::default(),
          created_at: 0,
      }
  }
  ```

  - **`test_pipeline_runs`**: Call `run_pipeline` with a single thought and test config → returns `PipelineResult` with no panics
  - **`test_observer_panic_produces_warning`**: Create a deliberately panicking observer (implement Observer trait with `panic!("test")`), add to observers vec, run pipeline → `panic_warnings` contains "ANALYZER-PANIC"
  - **`test_observations_merge`**: Create Depth, Budget, Bias observations → merge all 3 → verify all fields populated
  - **`test_observations_drain_alerts`**: Merge a Depth observation with 2 alerts → `drain_alerts()` returns 2 alerts, second call returns 0
  - **`test_confidence_always_returns_score`**: (Deferred to Agent 2 — needs ConfidenceEvaluator)

---

### Task 3: Implement DepthObserver

- **Description**: Topic overlap (prev + initial), contradiction detection (antonym + negation-on-predicate + quantifier), shallow analysis. Produces both observations and alerts.
- **Acceptance Criteria**:
  - [ ] Computes `prev_overlap` (current vs previous thought on branch) using `normalized_levenshtein`
  - [ ] Computes `initial_overlap` (current vs thought 1 on branch), `None` if this IS thought 1
  - [ ] Contradiction detection: antonym pairs, negation-on-predicate, quantifier conflicts
  - [ ] No standalone negation count diff (removed — causes false positives on refinements)
  - [ ] Shallow analysis: >50% of branch thoughts with prev overlap >0.7
  - [ ] Produces alerts: `PREMATURE_TOPIC_SWITCH`, `UNRESOLVED_CONTRADICTION`, `SHALLOW_ANALYSIS`
  - [ ] Excludes revision pairs from contradiction check
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/depth.rs`
- **Dependencies**: Task 2

- **Code — Static data**:
  ```rust
  use crate::config::Config;
  use crate::thought::{Alert, Severity, ThoughtInput, ThoughtRecord};
  use super::Observation;
  use std::sync::LazyLock;
  use strsim::normalized_levenshtein;

  static ANTONYM_PAIRS: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| vec![
      ("sync", "async"), ("blocking", "non-blocking"), ("stateful", "stateless"),
      ("mutable", "immutable"), ("eager", "lazy"), ("push", "pull"),
      ("static", "dynamic"), ("sequential", "parallel"), ("cached", "uncached"),
      ("persistent", "ephemeral"), ("local", "remote"),
      ("increase", "decrease"), ("add", "remove"), ("enable", "disable"),
      ("allow", "deny"), ("accept", "reject"), ("create", "destroy"),
      ("valid", "invalid"), ("safe", "unsafe"), ("strict", "loose"),
      ("explicit", "implicit"), ("required", "optional"),
  ]);

  static QUANTIFIER_CONFLICTS: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| vec![
      ("all", "none"), ("always", "never"), ("every", "no"),
      ("must", "may not"), ("required", "optional"),
  ]);

  static NEGATIONS: &[&str] = &["not ", "no ", "never ", "cannot ", "shouldn't ", "won't ", "don't "];
  ```

- **Code — DepthObserver**:
  ```rust
  pub struct DepthObserver;

  impl super::Observer for DepthObserver {
      fn observe(&self, input: &ThoughtInput, records: &[ThoughtRecord], _config: &Config) -> Observation {
          let branch_records: Vec<&ThoughtRecord> = records.iter()
              .filter(|r| r.input.branch_id == input.branch_id)
              .collect();

          let mut alerts = Vec::new();

          // Compute prev_overlap
          let prev_overlap = branch_records.last()
              .map(|prev| normalized_levenshtein(&input.thought, &prev.input.thought))
              .unwrap_or(0.0);

          // Compute initial_overlap (vs thought 1 on this branch)
          let initial_overlap = branch_records.first()
              .filter(|_| !branch_records.is_empty() && input.thought_number > 1)
              .map(|first| normalized_levenshtein(&input.thought, &first.input.thought));

          // Topic switch alert
          if !branch_records.is_empty() && prev_overlap < 0.3 {
              alerts.push(Alert {
                  analyzer: "depth".into(),
                  kind: "PREMATURE_TOPIC_SWITCH".into(),
                  severity: Severity::Medium,
                  message: format!("Topic overlap {:.2} with previous thought — jumped topic without finishing.", prev_overlap),
              });
          }

          // Contradiction detection across all branch thought pairs
          let mut contradictions = Vec::new();
          for record in &branch_records {
              // Skip if this is a revision of the other
              if input.is_revision && input.revises_thought == Some(record.input.thought_number) {
                  continue;
              }
              let sim = normalized_levenshtein(&input.thought, &record.input.thought);
              if sim > 0.7 && detect_contradiction(&input.thought, &record.input.thought) {
                  contradictions.push((record.input.thought_number, input.thought_number));
              }
          }

          if !contradictions.is_empty() {
              alerts.push(Alert {
                  analyzer: "depth".into(),
                  kind: "UNRESOLVED_CONTRADICTION".into(),
                  severity: Severity::High,
                  message: format!("Contradicting thoughts detected: {:?}. Revise or acknowledge the change.", contradictions),
              });
          }

          // Shallow analysis: >50% of branch thoughts have high overlap with predecessor
          let shallow = if branch_records.len() >= 2 {
              let high_overlap_count = branch_records.windows(2)
                  .filter(|w| normalized_levenshtein(&w[0].input.thought, &w[1].input.thought) > 0.7)
                  .count();
              // Also check current thought vs last record
              let current_high = prev_overlap > 0.7;
              let total_pairs = branch_records.len(); // pairs = records.len() (current is Nth, so N-1 pair windows + 1 current)
              let high_count = high_overlap_count + if current_high { 1 } else { 0 };
              high_count as f64 / total_pairs as f64 > 0.5
          } else {
              false
          };

          if shallow && branch_records.len() >= 3 {
              alerts.push(Alert {
                  analyzer: "depth".into(),
                  kind: "SHALLOW_ANALYSIS".into(),
                  severity: Severity::Medium,
                  message: "Over 50% of thoughts rephrase previous ones. Add new evidence or perspectives.".into(),
              });
          }

          Observation::Depth { prev_overlap, initial_overlap, contradictions, shallow, alerts }
      }
  }
  ```

- **Code — detect_contradiction()**:
  ```rust
  fn detect_contradiction(thought_a: &str, thought_b: &str) -> bool {
      let a = thought_a.to_lowercase();
      let b = thought_b.to_lowercase();

      // Layer 1: Domain antonym pairs + negation-on-predicate
      for (word_a, word_b) in ANTONYM_PAIRS.iter() {
          let a_has_first = a.contains(word_a);
          let a_has_second = a.contains(word_b);
          let b_has_first = b.contains(word_a);
          let b_has_second = b.contains(word_b);

          // Direct antonym swap
          if (a_has_first && b_has_second) || (a_has_second && b_has_first) {
              return true;
          }

          // Negation-on-predicate: same word, one negated
          if a_has_first && b_has_first {
              let a_negated = NEGATIONS.iter().any(|neg| {
                  a.find(neg).map_or(false, |pos| a[pos..].contains(word_a))
              });
              let b_negated = NEGATIONS.iter().any(|neg| {
                  b.find(neg).map_or(false, |pos| b[pos..].contains(word_a))
              });
              if a_negated != b_negated {
                  return true;
              }
          }
      }

      // Layer 2: Quantifier conflicts
      for (q_a, q_b) in QUANTIFIER_CONFLICTS.iter() {
          if (a.contains(q_a) && b.contains(q_b)) || (a.contains(q_b) && b.contains(q_a)) {
              return true;
          }
      }

      false
  }
  ```

- **Test Cases** (file: `src/analyzers/depth.rs` `#[cfg(test)]`):
  - **`test_prev_overlap_high_similarity`**: Two nearly identical thoughts → `prev_overlap > 0.7`
  - **`test_prev_overlap_low_similarity`**: Two unrelated thoughts → `prev_overlap < 0.3`
  - **`test_initial_overlap_computed`**: Thought 3 vs thought 1 → `initial_overlap` is `Some(value)`
  - **`test_initial_overlap_none_on_first_thought`**: No records → `initial_overlap` is `None`
  - **`test_topic_switch_fires`**: prev_overlap < 0.3 → alerts contain `PREMATURE_TOPIC_SWITCH`
  - **`test_antonym_contradiction`**: "use sync approach" vs "use async approach" with high strsim → `UNRESOLVED_CONTRADICTION`
  - **`test_negation_on_predicate_contradiction`**: "should use caching" vs "should not use caching" → `UNRESOLVED_CONTRADICTION`
  - **`test_quantifier_conflict`**: "always validate input" vs "never validate input" → `UNRESOLVED_CONTRADICTION`
  - **`test_no_contradiction_different_topics`**: Two unrelated thoughts (low strsim) → no contradiction
  - **`test_refinement_not_contradiction`**: "use PostgreSQL" → "use PostgreSQL, not Redis for caching" → no contradiction (negation doesn't flip a predicate in the antonym list)
  - **`test_shallow_analysis_fires`**: 4 records all highly similar → `SHALLOW_ANALYSIS` alert
  - **`test_revision_excluded_from_contradiction`**: Two contradicting thoughts but `is_revision=true` and `revises_thought` points to the other → no contradiction
  - **`test_depth_alerts_in_observation`**: Verify alerts vec in Observation::Depth is populated and accessible after merge

---

### Task 4: Implement BudgetObserver

- **Description**: Observation-only — returns budget data from config resolution. No alerts (warning engine owns those).
- **Acceptance Criteria**:
  - [ ] Returns `Observation::Budget { used, max, category }` from `config.resolve_budget()`
  - [ ] No alerts produced
  - [ ] Falls back to `(thought_number, 5, "standard")` when resolve_budget returns None
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/budget.rs`
- **Dependencies**: Task 2

- **Code**:
  ```rust
  use crate::config::Config;
  use crate::thought::{ThoughtInput, ThoughtRecord};
  use super::Observation;

  pub struct BudgetObserver;

  impl super::Observer for BudgetObserver {
      fn observe(&self, input: &ThoughtInput, _records: &[ThoughtRecord], config: &Config) -> Observation {
          let (used, max, category) = match config.resolve_budget(input.thinking_mode.as_deref()) {
              Some((_, max, tier)) => (input.thought_number, max, tier),
              None => (input.thought_number, 5, "standard".into()),
          };
          Observation::Budget { used, max, category }
      }
  }
  ```

- **Test Cases** (file: `src/analyzers/budget.rs` `#[cfg(test)]`):
  - **`test_budget_observation_architecture`**: mode="architecture" → `(thought_number, 8, "deep")`
  - **`test_budget_observation_implementation`**: mode="implementation" → `(thought_number, 3, "minimal")`
  - **`test_budget_unknown_mode`**: mode="nonexistent" → `(thought_number, 5, "standard")`
  - **`test_budget_no_alerts`**: Verify observation produces no alerts when merged

---

### Task 5: Implement BiasObserver

- **Description**: Detects 5 cognitive biases (first match wins). Uses domain antonym pairs, keyword matching, two-keyword sunk cost detection.
- **Acceptance Criteria**:
  - [ ] Anchoring: `normalized_levenshtein > 0.75` between thought 1 and current conclusion, no branches
  - [ ] Confirmation: all confirming keywords present, zero counter-argument keywords
  - [ ] Sunk cost: SET_A + SET_B two-keyword requirement, no question word = not tradeoff
  - [ ] Availability: entity from `config.components.valid` in >40% of thoughts. Returns false when components empty.
  - [ ] Overconfidence (timing): confidence > 80 AND thought_number/total_thoughts < 0.5
  - [ ] First match wins — returns single `Option<String>`
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/bias.rs`
- **Dependencies**: Task 2

- **Code — Static data**:
  ```rust
  use crate::config::Config;
  use crate::thought::{ThoughtInput, ThoughtRecord};
  use super::Observation;
  use std::collections::{HashMap, HashSet};
  use std::sync::LazyLock;
  use strsim::normalized_levenshtein;

  static COUNTER_ARGUMENT_KEYWORDS: LazyLock<Vec<&str>> = LazyLock::new(|| vec![
      "however", "but", "contrary", "alternatively", "problem with this",
      "weakness", "downside", "risk", "what if", "on the other hand",
      "counter", "drawback", "issue with",
  ]);

  static CONFIRMING_KEYWORDS: LazyLock<Vec<&str>> = LazyLock::new(|| vec![
      "confirms", "supports", "as expected", "validates", "consistent with",
      "this proves", "clearly",
  ]);

  static SUNK_COST_ANCHORS: LazyLock<Vec<&str>> = LazyLock::new(|| vec![
      "already built", "already implemented", "already written", "already invested",
      "we've spent", "can't throw away", "can't discard", "too much work",
      "not worth rewriting", "significant effort",
  ]);

  static SUNK_COST_CONTINUATIONS: LazyLock<Vec<&str>> = LazyLock::new(|| vec![
      "so we should keep", "therefore we continue", "better to keep",
      "might as well", "instead of starting over", "instead of rewriting",
      "push through", "commit to", "worth continuing",
  ]);
  ```

- **Code — BiasObserver** (implement `observe` with 5 checks, first match wins, return `Observation::Bias { detected }`):

  Each bias check is a private function returning `Option<&'static str>`:
  - `detect_anchoring(input, records)` — strsim thought 1 vs current > 0.75, no branches in records
  - `detect_confirmation(input, records)` — has confirming keywords, zero counter-argument keywords across all thoughts
  - `detect_sunk_cost(input, records)` — SET_A + SET_B in same/adjacent thought, no question word
  - `detect_availability(input, records, config)` — entity repetition >40% of thoughts, returns None if `config.components.valid.is_empty()`
  - `detect_overconfidence(input)` — confidence > 80 AND progress < 50%

  The `observe` method calls them in order, returns first `Some`.

- **Test Cases** (file: `src/analyzers/bias.rs` `#[cfg(test)]`):
  - **`test_anchoring_detected`**: Thought 1 = "Use Redis for caching", current (thought 5) = "Redis is the right caching solution" (strsim >0.75), no branches → `Some("anchoring")`
  - **`test_confirmation_bias_detected`**: All thoughts contain "confirms", "supports" — zero counter-arguments → `Some("confirmation")`
  - **`test_sunk_cost_detected`**: Thought says "already built the MongoDB schemas, so we should keep using it" → `Some("sunk_cost")`
  - **`test_sunk_cost_false_positive_question`**: "Already built MongoDB, but should we switch to PostgreSQL?" contains question word → `None`
  - **`test_availability_detected`**: Component "redis" in 3 of 5 thoughts (>40%) → `Some("availability")`
  - **`test_availability_no_components_no_detection`**: `config.components.valid = vec![]` → `None` (not `Some("availability")`)
  - **`test_overconfidence_timing`**: confidence=90, thought 2 of 8 (25% progress) → `Some("overconfidence")`
  - **`test_no_bias_clean_reasoning`**: Diverse thoughts with branches and counter-arguments → `None`
  - **`test_first_match_wins`**: Thought triggers both anchoring and confirmation → returns `"anchoring"` (checked first)

---

### Task 6: Verify all tests pass

- **Description**: Run `cargo test` and verify all existing + new tests pass.
- **Acceptance Criteria**:
  - [ ] All ~106 existing tests pass
  - [ ] New pipeline tests pass (~4 in mod.rs)
  - [ ] New depth tests pass (~13)
  - [ ] New budget tests pass (~4)
  - [ ] New bias tests pass (~9)
  - [ ] `cargo check` passes with no errors
- **Dependencies**: Tasks 1-5
- **Run**: `cargo test`

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]`, `cargo test`
- **Structure**: Tests inline in each analyzer file
- **Coverage**: ~30 new tests across mod.rs, depth.rs, budget.rs, bias.rs
- **Shared helpers**: `test_input()` and `test_record()` in `mod.rs` as `pub(crate)` for use by all analyzer tests
- **Run**: `cargo test`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| strsim normalized_levenshtein thresholds too sensitive/insensitive | Medium | Medium | Research-backed thresholds. Tunable constants. | Test with varied thought text |
| Rayon thread pool startup on first pipeline call | Low | Low | One-time ~1ms, acceptable | Manual timing |
| Antonym pairs miss contradictions | Medium | Low | 22 pairs extensible. Trace review catches rest. | test_no_contradiction_different_topics |
| Sunk cost two-keyword false positives despite question-word filter | Medium | Low | First-match-wins limits noise to 1 bias per thought | test_sunk_cost_false_positive_question |
| catch_unwind + AssertUnwindSafe | Low | Medium | All analyzers are stateless — no invariants to break | test_observer_panic_produces_warning |

## Success Criteria

- [ ] Pipeline compiles and runs with rayon parallel execution
- [ ] 3 observers produce correct observations
- [ ] Depth observer fires 3 alert types correctly
- [ ] Budget observer returns data only (no alerts)
- [ ] Bias observer catches 5 biases with first-match-wins
- [ ] catch_unwind isolates panics and produces warnings
- [ ] All ~136 tests pass (106 existing + ~30 new)

## Implementation Notes

- **Do NOT modify `src/thought.rs`** — Agent 2 handles Clone derives and pipeline integration
- **Do NOT modify evaluator files** (`confidence.rs`, `sycophancy.rs`) — Agent 2 handles those
- **The evaluator vec in `run_pipeline()` is intentionally empty** — Agent 2 will uncomment evaluator lines
- **`test_input()` and `test_record()` helpers** must be `pub(crate)` so Agent 2's evaluator tests can use them
- **Import paths**: Observers use `use super::Observation;` and `use super::Observer;`
- **`ThoughtResult::default()`** exists (derive Default on ThoughtResult) — use it in test_record()
- **The `COUNTER_ARGUMENT_KEYWORDS` list in bias.rs** is also needed by the sycophancy evaluator (Agent 2). Since they're in different files, Agent 2 will define its own copy or Agent 1 can make it `pub(crate)` in mod.rs. **Decision: define shared keyword lists in `mod.rs` as `pub(crate)` statics. Observers and evaluators import from there.**

  Move these to mod.rs:
  - `COUNTER_ARGUMENT_KEYWORDS`
  - `CONFIRMING_KEYWORDS`
  - `HARMFUL_WORDS` (for confidence evaluator)

  Keep analyzer-specific ones local:
  - `ANTONYM_PAIRS`, `QUANTIFIER_CONFLICTS`, `NEGATIONS` in depth.rs
  - `SUNK_COST_ANCHORS`, `SUNK_COST_CONTINUATIONS` in bias.rs
