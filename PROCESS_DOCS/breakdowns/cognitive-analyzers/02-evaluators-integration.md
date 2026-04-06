# Build Agent 2: Evaluators + Integration

## Dependencies

Depends on `01-pipeline-observers.md` — needs traits, types, `Observations` struct, shared keyword lists in `mod.rs`.

## Overview

- **Objective**: Implement the 2 evaluators (confidence, sycophancy), add `Clone` to thought types, clone branch records in Phase 1, wire `run_pipeline()` into `process_thought()` Phase 2, populate all `WireResponse` analyzer fields.
- **Scope**:
  - Includes: `src/analyzers/confidence.rs`, `src/analyzers/sycophancy.rs`, `src/analyzers/mod.rs` (add evaluators to run_pipeline), `src/thought.rs` (Clone derives, Phase 1 cloning, Phase 2 pipeline call)
  - Excludes: Observer files (Agent 1), `src/warnings.rs`, `src/mcp.rs`, `src/llm.rs`
- **Dependencies**:
  - Agent 1 output: `src/analyzers/mod.rs` (Observer/Evaluator traits, Observation/Observations types, EvalOutput, PipelineResult, run_pipeline skeleton, shared keyword statics)
  - Existing: `src/thought.rs` (ThoughtInput, ThoughtRecord, ThoughtResult, Alert, Severity, WireResponse, process_thought)
- **Estimated Complexity**: Medium — 2 evaluator implementations + thought.rs integration

## Technical Approach

### Evaluator Contract
Each evaluator implements `Evaluator: Send + Sync` and returns `EvalOutput`:
- `ConfidenceEvaluator` → `EvalOutput::Confidence { calculated: f64, alert: Option<Alert> }` — always returns calculated score
- `SycophancyEvaluator` → `EvalOutput::Sycophancy { pattern: Option<String>, alert: Option<Alert> }` — pattern name or None

### Similarity Function
All similarity: `strsim::normalized_levenshtein`. Threshold 0.7 for sycophancy's CONFIRMATION_ONLY pattern uses `observations.initial_overlap` (thought 1 vs current), NOT `prev_overlap`.

---

## Task Breakdown

### Task 1: Implement ConfidenceEvaluator

- **Description**: Scoring rubric with 2x2 hedging×evidence matrix. Always returns calculated confidence score. Fires OVERCONFIDENCE alert when gap > 25.
- **Acceptance Criteria**:
  - [ ] Scoring rubric: evidence (0-30), alternatives (0-25), contradictions (-20), substance (0-15), bias avoidance (0-10)
  - [ ] Max 80 raw, normalized to 0-100
  - [ ] Substance uses 2x2 matrix: hedging × evidence
  - [ ] Harmful words from `HARMFUL_WORDS` in mod.rs (arXiv:2508.15842 validated)
  - [ ] OVERCONFIDENCE alert when `|reported - calculated| > config.thresholds.confidence_gap`
  - [ ] Always returns `EvalOutput::Confidence { calculated, alert }` — even when no alert, `calculated` is set
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/confidence.rs`
- **Dependencies**: Agent 1 (mod.rs types + shared statics)

- **Code — ConfidenceEvaluator**:
  ```rust
  use crate::config::Config;
  use crate::thought::{Alert, Severity, ThoughtInput, ThoughtRecord};
  use super::{Observations, EvalOutput, HARMFUL_WORDS};

  pub struct ConfidenceEvaluator;

  impl super::Evaluator for ConfidenceEvaluator {
      fn evaluate(
          &self,
          input: &ThoughtInput,
          records: &[ThoughtRecord],
          observations: &Observations,
          config: &Config,
      ) -> EvalOutput {
          let mut score: i32 = 0;

          // Evidence: 10 pts per citation, max 30
          let evidence_pts = (input.evidence.len() as i32 * 10).min(30);
          score += evidence_pts;

          // Alternatives: 25 pts if any branching in trace
          let has_branches = records.iter().any(|r| r.input.branch_from_thought.is_some());
          if has_branches {
              score += 25;
          }

          // Contradictions penalty: -20 if unresolved
          if !observations.contradictions.is_empty() {
              score -= 20;
          }

          // Substance: 2x2 hedging × evidence matrix
          score += substance_score(&input.thought, input.evidence.len());

          // Bias avoidance: 10 pts if no bias detected
          if observations.bias_detected.is_none() {
              score += 10;
          }

          // Normalize: max raw = 80, scale to 0-100
          let calculated = ((score.max(0) as f64 / 80.0) * 100.0).min(100.0);

          // Check for overconfidence
          let alert = input.confidence.and_then(|reported| {
              let gap = (reported - calculated).abs();
              if gap > config.thresholds.confidence_gap {
                  Some(Alert {
                      analyzer: "confidence".into(),
                      kind: "OVERCONFIDENCE".into(),
                      severity: Severity::High,
                      message: format!(
                          "Reported {:.0}% but evidence supports {:.0}% (gap: {:.0}). Cite more evidence or lower confidence.",
                          reported, calculated, gap
                      ),
                  })
              } else {
                  None
              }
          });

          EvalOutput::Confidence { calculated, alert }
      }
  }

  fn substance_score(thought: &str, evidence_count: usize) -> i32 {
      let thought_lower = thought.to_lowercase();
      let harmful_count = HARMFUL_WORDS.iter()
          .filter(|w| thought_lower.contains(**w))
          .count();
      let has_hedging = harmful_count >= 2;
      let has_evidence = evidence_count > 0;

      match (has_hedging, has_evidence) {
          (false, true)  => 15, // confident and grounded
          (true,  true)  => 10, // calibrated uncertainty
          (false, false) => 5,  // overconfident (no hedge, no evidence)
          (true,  false) => 0,  // uncertain and ungrounded
      }
  }
  ```

- **Test Cases** (file: `src/analyzers/confidence.rs` `#[cfg(test)]`):

  Use `test_input()` and `test_record()` from `super::test_input` / `super::test_record`.

  - **`test_high_evidence_high_score`**: input with 3 evidence items, branch in records, no contradictions, no bias → score = (30 + 25 + 0 + 15 + 10) / 80 * 100 = 100
  - **`test_no_evidence_low_score`**: input with 0 evidence, no branches, no contradictions, hedging words "I think this is probably fine" → score = (0 + 0 + 0 + 0 + 10) / 80 * 100 = 12.5
  - **`test_overconfidence_alert_fires`**: reported=90, calculated ~12.5, gap 77.5 > 25 → alert with kind "OVERCONFIDENCE"
  - **`test_overconfidence_within_gap_no_alert_but_score_returned`**: reported=50, calculated=40, gap=10 < 25 → no alert, but `calculated` is still `Some(40.0)` (always returned)
  - **`test_substance_hedging_no_evidence`**: "I think this is probably maybe right" (3 harmful words) + 0 evidence → substance = 0
  - **`test_substance_hedging_with_evidence`**: "I think this is probably right" + 1 evidence → substance = 10
  - **`test_substance_no_hedging_with_evidence`**: "PostgreSQL handles ACID transactions" + 2 evidence → substance = 15
  - **`test_substance_no_hedging_no_evidence`**: "PostgreSQL is the answer" + 0 evidence → substance = 5
  - **`test_contradiction_penalty`**: observations with contradictions vec non-empty → score reduced by 20 raw pts
  - **`test_branch_alternatives_bonus`**: records with `branch_from_thought` set → score gets +25 raw pts

---

### Task 2: Implement SycophancyEvaluator

- **Description**: 3 sycophancy patterns (first match wins). Uses `initial_overlap` from observations for CONFIRMATION_ONLY. Checks counter-argument keywords in intermediate thoughts.
- **Acceptance Criteria**:
  - [ ] PREMATURE_AGREEMENT (Severity::Medium): thoughts 1-2 agree, no challenge keywords
  - [ ] NO_SELF_CHALLENGE (Severity::Medium): 3+ consecutive thoughts without branch/revision
  - [ ] CONFIRMATION_ONLY (Severity::High): `initial_overlap > 0.7` AND zero counter-argument keywords in intermediate thoughts AND zero revisions AND zero branches
  - [ ] Uses `observations.initial_overlap` (NOT `prev_overlap`)
  - [ ] Counter-argument keywords from `COUNTER_ARGUMENT_KEYWORDS` in mod.rs
  - [ ] Returns `EvalOutput::Sycophancy { pattern, alert }`
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/sycophancy.rs`
- **Dependencies**: Agent 1 (mod.rs types + shared statics)

- **Code — SycophancyEvaluator**:
  ```rust
  use crate::config::Config;
  use crate::thought::{Alert, Severity, ThoughtInput, ThoughtRecord};
  use super::{Observations, EvalOutput, COUNTER_ARGUMENT_KEYWORDS};

  pub struct SycophancyEvaluator;

  impl super::Evaluator for SycophancyEvaluator {
      fn evaluate(
          &self,
          input: &ThoughtInput,
          records: &[ThoughtRecord],
          observations: &Observations,
          _config: &Config,
      ) -> EvalOutput {
          // Pattern 1: Premature agreement (check at thought 2)
          if input.thought_number == 2 && records.len() >= 1 {
              let t1 = &records[0].input.thought.to_lowercase();
              let t2 = input.thought.to_lowercase();
              let t1_has_challenge = COUNTER_ARGUMENT_KEYWORDS.iter().any(|k| t1.contains(k));
              let t2_has_challenge = COUNTER_ARGUMENT_KEYWORDS.iter().any(|k| t2.contains(k));
              if !t1_has_challenge && !t2_has_challenge {
                  return EvalOutput::Sycophancy {
                      pattern: Some("PREMATURE_AGREEMENT".into()),
                      alert: Some(Alert {
                          analyzer: "sycophancy".into(),
                          kind: "PREMATURE_AGREEMENT".into(),
                          severity: Severity::Medium,
                          message: "First 2 thoughts agree without challenging the premise. Consider counter-arguments.".into(),
                      }),
                  };
              }
          }

          // Pattern 2: No self-challenge (3+ consecutive without branch/revision)
          if input.thought_number >= 3 {
              let branch_records: Vec<&ThoughtRecord> = records.iter()
                  .filter(|r| r.input.branch_id == input.branch_id)
                  .collect();
              let last_3 = branch_records.iter().rev().take(3);
              let any_challenge = last_3.into_iter()
                  .any(|r| r.input.is_revision || r.input.branch_from_thought.is_some());
              if !any_challenge {
                  return EvalOutput::Sycophancy {
                      pattern: Some("NO_SELF_CHALLENGE".into()),
                      alert: Some(Alert {
                          analyzer: "sycophancy".into(),
                          kind: "NO_SELF_CHALLENGE".into(),
                          severity: Severity::Medium,
                          message: "3+ thoughts without branching or revision. Challenge your own reasoning.".into(),
                      }),
                  };
              }
          }

          // Pattern 3: Confirmation-only conclusion (most dangerous)
          if !input.next_thought_needed && records.len() >= 3 {
              let initial_sim = observations.initial_overlap.unwrap_or(0.0);
              if initial_sim > 0.7 {
                  let any_revisions = records.iter().any(|r| r.input.is_revision);
                  let any_branches = records.iter().any(|r| r.input.branch_from_thought.is_some());

                  // Check for counter-arguments in ALL intermediate thoughts
                  let has_counter_argument = records.iter()
                      .any(|r| {
                          let lower = r.input.thought.to_lowercase();
                          COUNTER_ARGUMENT_KEYWORDS.iter().any(|k| lower.contains(k))
                      });

                  if !any_revisions && !any_branches && !has_counter_argument {
                      return EvalOutput::Sycophancy {
                          pattern: Some("CONFIRMATION_ONLY".into()),
                          alert: Some(Alert {
                              analyzer: "sycophancy".into(),
                              kind: "CONFIRMATION_ONLY".into(),
                              severity: Severity::High,
                              message: format!(
                                  "Final conclusion matches initial hypothesis ({:.0}% similar) with zero course corrections. This is confirmation bias, not analysis.",
                                  initial_sim * 100.0
                              ),
                          }),
                      };
                  }
              }
          }

          EvalOutput::Sycophancy { pattern: None, alert: None }
      }
  }
  ```

- **Test Cases** (file: `src/analyzers/sycophancy.rs` `#[cfg(test)]`):
  - **`test_premature_agreement_fires`**: thought_number=2, records=[thought 1 with no challenge words], current thought has no challenge words → PREMATURE_AGREEMENT
  - **`test_premature_agreement_with_challenge_ok`**: thought_number=2, but thought 1 contains "however" → no alert
  - **`test_no_self_challenge_fires`**: thought_number=4, last 3 records have no revision/branch → NO_SELF_CHALLENGE
  - **`test_no_self_challenge_with_revision_ok`**: thought_number=4, but one of last 3 has `is_revision=true` → no alert
  - **`test_confirmation_only_fires_initial_overlap`**: next_thought_needed=false, `observations.initial_overlap=Some(0.85)`, 4 records with no revisions/branches/counter-arguments → CONFIRMATION_ONLY
  - **`test_confirmation_only_with_counter_argument_ok`**: Same as above but one record contains "however" → no alert
  - **`test_confirmation_only_with_branch_ok`**: Same but one record has `branch_from_thought=Some(2)` → no alert
  - **`test_confirmation_only_uses_initial_not_prev_overlap`**: `observations.initial_overlap=Some(0.3)` (low), `observations.prev_overlap=Some(0.9)` (high) → no CONFIRMATION_ONLY (proves it uses initial, not prev)

---

### Task 3: Wire evaluators into run_pipeline()

- **Description**: Uncomment the evaluator vec entries in `run_pipeline()` that Agent 1 left empty.
- **Acceptance Criteria**:
  - [ ] `run_pipeline()` evaluators vec contains ConfidenceEvaluator and SycophancyEvaluator
  - [ ] `cargo check` passes
- **Files to Modify**: `src/analyzers/mod.rs`
- **Dependencies**: Tasks 1, 2

- **Code**: In `run_pipeline()`, replace the empty evaluators vec:
  ```rust
  // REPLACE:
  let evaluators: Vec<(&str, Box<dyn Evaluator>)> = vec![
      // Agent 2 adds: ("confidence", Box::new(confidence::ConfidenceEvaluator)),
      // Agent 2 adds: ("sycophancy", Box::new(sycophancy::SycophancyEvaluator)),
  ];

  // WITH:
  let evaluators: Vec<(&str, Box<dyn Evaluator>)> = vec![
      ("confidence", Box::new(confidence::ConfidenceEvaluator)),
      ("sycophancy", Box::new(sycophancy::SycophancyEvaluator)),
  ];
  ```

---

### Task 4: Add Clone derives and wire pipeline into thought.rs

- **Description**: Add `Clone` to `ThoughtResult` and `ThoughtRecord`. Clone branch-filtered records in Phase 1 for analyzer use. Call `run_pipeline()` in Phase 2 and populate `WireResponse` fields.
- **Acceptance Criteria**:
  - [ ] `#[derive(Clone)]` added to `ThoughtResult`
  - [ ] `ThoughtRecord` derives `Clone`
  - [ ] Phase 1: clone branch-filtered records into `branch_records: Vec<ThoughtRecord>` in `TraceSnapshot`
  - [ ] Phase 2: `run_pipeline()` called with `&input, &snapshot.branch_records, &self.config`
  - [ ] `WireResponse` fields populated: `alerts`, `confidence_calculated`, `confidence_gap`, `depth_overlap`, `bias_detected`, `sycophancy`
  - [ ] Panic warnings from pipeline appended to `warnings` vec
  - [ ] All existing tests still pass
  - [ ] `cargo check` passes
- **Files to Modify**: `src/thought.rs`
- **Dependencies**: Task 3

- **Code — Clone derives**:
  ```rust
  // Change ThoughtResult derive line from:
  #[derive(Debug, Serialize, Deserialize, Default)]
  // To:
  #[derive(Debug, Serialize, Deserialize, Default, Clone)]

  // Add derive to ThoughtRecord:
  #[derive(Clone)]
  pub struct ThoughtRecord {
  ```

- **Code — TraceSnapshot change**: Add field:
  ```rust
  pub branch_records: Vec<ThoughtRecord>,
  ```

- **Code — Phase 1 extraction** (inside write lock, after existing branch filtering):
  ```rust
  let branch_records: Vec<ThoughtRecord> = trace.thoughts
      .iter()
      .filter(|t| t.input.branch_id == input.branch_id)
      .cloned()
      .collect();
  ```
  Add to `TraceSnapshot { ..., branch_records }`.

- **Code — Phase 2 integration** (replace `alerts: vec![]` and `None` fields):
  ```rust
  use crate::analyzers::run_pipeline;

  // After warnings generation:
  let pipeline = run_pipeline(&input, &snapshot.branch_records, &self.config);

  // Append pipeline panic warnings
  let mut warnings = generate_warnings(&input, &snapshot.recent_progress, &self.config);
  warnings.extend(pipeline.panic_warnings);

  Ok(WireResponse {
      // ... existing fields ...
      warnings,
      alerts: pipeline.alerts,
      confidence_reported: input.confidence,
      confidence_calculated: pipeline.confidence_calculated,
      confidence_gap: match (input.confidence, pipeline.confidence_calculated) {
          (Some(reported), Some(calculated)) => Some((reported - calculated).abs()),
          _ => None,
      },
      bias_detected: pipeline.observations.bias_detected.clone(),
      sycophancy: pipeline.sycophancy_pattern,
      depth_overlap: pipeline.observations.prev_overlap,
      // ... rest unchanged ...
  })
  ```

- **Code — Update existing test helpers**: Any test that constructs `TraceSnapshot` must include `branch_records: vec![]`. Any test that constructs `WireResponse` must match updated field sources.

- **Test Cases** (file: `src/thought.rs` `#[cfg(test)]`):
  - **`test_process_thought_runs_pipeline`**: Send a thought through `process_thought()` → `WireResponse` has `confidence_calculated` populated (not None)
  - **`test_pipeline_alerts_in_wire_response`**: Send thought with shortcut language "I'm 95% sure this is fine" and 0 evidence → `alerts` contains OVERCONFIDENCE
  - **Update existing tests**: Add `branch_records: vec![]` to any TraceSnapshot construction in tests

---

### Task 5: Verify all tests pass

- **Description**: Run `cargo test` and verify everything works.
- **Acceptance Criteria**:
  - [ ] All ~106 existing tests pass
  - [ ] All ~30 new observer tests pass (Agent 1)
  - [ ] All ~10 new confidence tests pass
  - [ ] All ~8 new sycophancy tests pass
  - [ ] All ~4 new mod.rs tests pass
  - [ ] New thought.rs integration tests pass
  - [ ] `cargo check` with no errors
- **Dependencies**: Tasks 1-4
- **Run**: `cargo test`

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]`, `cargo test`
- **Structure**: Tests inline in `src/analyzers/confidence.rs`, `src/analyzers/sycophancy.rs`, `src/thought.rs`
- **Coverage**: ~20 new tests in this agent + Agent 1's ~30
- **Shared helpers**: Use `super::test_input()` and `super::test_record()` from `mod.rs` (Agent 1 provides these as `pub(crate)`)
- **Run**: `cargo test`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| Clone on ThoughtRecord breaks existing tests | Medium | Low | Add `branch_records: vec![]` to all TraceSnapshot constructions | cargo test |
| Confidence score normalization edge cases (negative raw score) | Low | Medium | `.max(0)` before normalization | test_contradiction_penalty |
| Sycophancy CONFIRMATION_ONLY uses wrong overlap | Low | High (design bug) | Design specifies `initial_overlap` explicitly. Test proves it. | test_confirmation_only_uses_initial_not_prev_overlap |
| EvalOutput match in run_pipeline doesn't compile after Agent 1 | Low | Medium | Agent 1 leaves evaluator vec empty but compiles. Agent 2 adds entries. | cargo check after Task 3 |
| process_thought() Phase 2 ordering conflicts with recap/adr | Low | Medium | Pipeline called after warnings, before WireResponse construction | test_process_thought_runs_pipeline |

## Success Criteria

- [ ] ConfidenceEvaluator always returns calculated score, fires OVERCONFIDENCE when gap > 25
- [ ] SycophancyEvaluator catches 3 patterns, uses `initial_overlap` for CONFIRMATION_ONLY
- [ ] Pipeline fully wired into `process_thought()` — all WireResponse analyzer fields populated
- [ ] Panic warnings from pipeline appear in `WireResponse.warnings`
- [ ] All ~160 tests pass (106 existing + ~30 observer + ~20 evaluator/integration)
- [ ] `cargo check` passes

## Implementation Notes

- **Agent 1 must complete first** — this agent depends on traits, types, and shared statics in `mod.rs`
- **`HARMFUL_WORDS` and `COUNTER_ARGUMENT_KEYWORDS`** are in `mod.rs` as `pub(crate)` statics (Agent 1 puts them there)
- **`observations.initial_overlap`** comes from the depth observer (Agent 1). It's `Option<f64>` — `None` on first thought. Sycophancy evaluator handles this with `.unwrap_or(0.0)`.
- **`confidence_gap` computation** happens in `thought.rs`, not in the evaluator. The evaluator returns the calculated score; `thought.rs` computes the absolute difference when both reported and calculated are present.
- **`ThoughtResult::default()`** already exists. Adding `Clone` derive doesn't break it.
- **Float comparison**: `(reported - calculated).abs()` — both are `f64`, this is fine for our precision needs.
