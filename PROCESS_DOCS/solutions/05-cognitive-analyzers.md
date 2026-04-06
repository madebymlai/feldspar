# Solution Design: Cognitive Analyzers

## Executive Summary

Five cognitive analyzers that evaluate every thought for overconfidence, sycophancy, cognitive biases, shallow analysis, and reasoning depth. Observer/Evaluator two-phase pipeline with rayon parallelism. All pure heuristics — `strsim::normalized_levenshtein`, keyword matching, domain antonym pairs, counting. Zero LLM calls. Microsecond-scale per thought. Research-backed thresholds with 9 cited papers.

## Rationale

| Decision | Rationale | Alternative | Why Rejected |
|----------|-----------|-------------|--------------|
| Struct-per-analyzer with trait | Testable independently, can hold config, implements Observer/Evaluator trait | Closures in a vec | Not testable in isolation, harder to read |
| `catch_unwind` per analyzer | One panic doesn't crash pipeline. Logged as warning. | Let panics propagate | Violates "best-effort" constraint |
| Rayon `par_iter` for both phases | User choice. Right foundation even if work is lightweight now. | Sequential loop | User explicitly chose rayon |
| Domain antonym pairs (not WordNet) | ~30 code/architecture pairs cover the domain. WordNet is a massive dep for marginal gain. | Full WordNet lookup | Too heavy, most pairs irrelevant for code reasoning |
| 2x2 hedging×evidence matrix | Research-validated (arXiv:2508.15842): harmful word detection MCC 0.354 outperforms self-reported confidence MCC 0.065 | Simple filler count | Misses the evidence interaction that determines whether hedging is calibrated or uncertain |
| Budget observer = observation only | AR review on issue #4 decided warning engine owns budget alerts. Analyzer provides data, not alerts. | Budget observer fires alerts | Duplicate alerts in `warnings[]` and `alerts[]` |
| Counter-argument keywords (unvalidated) | No paper validates a keyword list for CoT counter-argument detection. Best available heuristic from argumentation literature. v0.1 — expect iteration. | Skip counter-argument check | Sycophancy evaluator loses its strongest signal |
| `Clone` on ThoughtRecord | Analyzers need full branch-filtered records (thought texts for strsim, keywords). Lightweight — ThoughtInput and ThoughtResult already cloneable. | Extract only needed fields | Too many fields needed across 5 analyzers, extraction struct would be as complex as cloning |
| `strsim::normalized_levenshtein` | Conservative, edit-distance-based, well-understood score distribution. 0.3/0.7 thresholds defensible. | `jaro_winkler` | Produces higher scores for loosely similar strings — thresholds would need recalibration |
| Named tuples for observer/evaluator dispatch | `("depth", &depth_obs)` prevents wrong panic labels if reordered | Index-based `["depth", "budget", "bias"][i]` | Fragile coupling, silent bugs on reorder |
| Negation merged into antonym detection | Raw negation count diff false-positives on refinements ("use X" → "use X, not Y"). Only fire when negation flips a domain predicate. | Standalone negation count | "We should use PostgreSQL, not Redis" would false-positive as contradiction |
| Thresholds are structural constants | Most thresholds (0.3, 0.7, 0.75, 40%, etc.) are hardcoded, not config-driven. Only `confidence_gap` comes from config. Documented honestly — not configurable by design. | Add all to config | Config complexity explosion for rarely-tuned values |

## Technology Stack

- `rayon` crate — **new dependency**, add `rayon = "1.10"` to `Cargo.toml`
- `strsim` crate (already in Cargo.toml)
- `std::sync::LazyLock` for static keyword/antonym sets
- `std::panic::catch_unwind` for fault isolation

## Architecture

### Data Flow

```
ThoughtInput + [ThoughtRecord] + Config
    │
    ├── Phase 1: Observers (rayon par_iter, catch_unwind each)
    │   ├── DepthObserver  → Observation::Depth { overlap, contradictions, shallow }
    │   ├── BudgetObserver → Observation::Budget { used, max, category }
    │   └── BiasObserver   → Observation::Bias { detected }
    │
    ├── Merge → Observations struct
    │
    ├── Phase 2: Evaluators (rayon par_iter, catch_unwind each)
    │   ├── ConfidenceEvaluator → Option<Alert> (OVERCONFIDENCE)
    │   └── SycophancyEvaluator → Option<Alert> (PREMATURE_AGREEMENT | NO_SELF_CHALLENGE | CONFIRMATION_ONLY)
    │
    └── (Vec<Alert>, Observations) → process_thought() Phase 2
                                        │
                                        ├── wire.alerts = alerts
                                        ├── wire.depth_overlap = observations.prev_overlap
                                        ├── wire.confidence_calculated = calculated score
                                        ├── wire.confidence_gap = |reported - calculated|
                                        ├── wire.bias_detected = observations.bias_detected
                                        └── wire.sycophancy = sycophancy pattern name
```

### Component Catalog

| Component | File | Purpose |
|-----------|------|---------|
| `Observer` trait | `src/analyzers/mod.rs` | `observe(input, records, config) -> Observation` |
| `Evaluator` trait | `src/analyzers/mod.rs` | `evaluate(input, records, observations, config) -> Option<Alert>` |
| `Observation` enum | `src/analyzers/mod.rs` | Data produced by observers |
| `Observations` struct | `src/analyzers/mod.rs` | Merged observer outputs, passed to evaluators |
| `run_pipeline()` | `src/analyzers/mod.rs` | Entry point: observers → merge → evaluators → collect |
| `DepthObserver` | `src/analyzers/depth.rs` | Topic overlap, contradiction detection, shallow analysis |
| `BudgetObserver` | `src/analyzers/budget.rs` | Budget tier observation (no alerts) |
| `BiasObserver` | `src/analyzers/bias.rs` | 5 cognitive biases |
| `ConfidenceEvaluator` | `src/analyzers/confidence.rs` | Scoring rubric, overconfidence alert |
| `SycophancyEvaluator` | `src/analyzers/sycophancy.rs` | 3 sycophancy patterns |

## Protocol/Schema

### Observation Enum

```rust
pub enum Observation {
    Depth {
        prev_overlap: f64,               // normalized_levenshtein with previous thought on branch
        initial_overlap: Option<f64>,     // normalized_levenshtein with thought 1 on branch (None if thought 1)
        contradictions: Vec<(u32, u32)>,  // pairs of contradicting thought numbers
        shallow: bool,                    // >50% rephrasing on branch with no new tokens
        alerts: Vec<Alert>,              // PREMATURE_TOPIC_SWITCH, UNRESOLVED_CONTRADICTION, SHALLOW_ANALYSIS
    },
    Budget {
        used: u32,
        max: u32,
        category: String,
    },
    Bias {
        detected: Option<String>,         // bias name or None
    },
}
```

### Observations Struct (merged, passed to evaluators)

```rust
pub struct Observations {
    pub prev_overlap: Option<f64>,
    pub initial_overlap: Option<f64>,     // thought 1 vs current — used by sycophancy evaluator
    pub contradictions: Vec<(u32, u32)>,
    pub shallow: bool,
    pub budget_used: u32,
    pub budget_max: u32,
    pub budget_category: String,
    pub bias_detected: Option<String>,
    pub pending_alerts: Vec<Alert>,       // alerts from observers (depth produces these)
}

impl Observations {
    pub fn drain_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.pending_alerts)
    }
}
```

### Evaluator Return Types

The `Evaluator` trait returns `Option<Alert>` for most evaluators. The confidence evaluator needs a richer return because it always produces a calculated score, not just an alert:

```rust
pub trait Observer: Send + Sync {
    fn observe(&self, input: &ThoughtInput, records: &[ThoughtRecord], config: &Config) -> Observation;
}

pub trait Evaluator: Send + Sync {
    fn evaluate(&self, input: &ThoughtInput, records: &[ThoughtRecord],
                observations: &Observations, config: &Config) -> EvalOutput;
}

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

### Pipeline Return

```rust
pub struct PipelineResult {
    pub alerts: Vec<Alert>,
    pub observations: Observations,
    pub confidence_calculated: Option<f64>,  // always present when confidence evaluator runs
    pub sycophancy_pattern: Option<String>,  // from sycophancy evaluator
    pub panic_warnings: Vec<String>,         // from catch_unwind failures
}
```

### Static Keyword Sets

```rust
// Domain antonym pairs for contradiction detection
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

// Quantifier conflicts
static QUANTIFIER_CONFLICTS: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| vec![
    ("all", "none"), ("always", "never"), ("every", "no"),
    ("must", "may not"), ("required", "optional"),
]);

// Counter-argument keywords (shared: sycophancy + confirmation bias)
static COUNTER_ARGUMENT_KEYWORDS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    ["however", "but", "contrary", "alternatively", "problem with this",
     "weakness", "downside", "risk", "what if", "on the other hand",
     "counter", "drawback", "issue with"].into_iter().collect()
});

// Confirming keywords (confirmation bias detection)
static CONFIRMING_KEYWORDS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    ["confirms", "supports", "as expected", "validates", "consistent with",
     "this proves", "clearly"].into_iter().collect()
});

// Harmful reasoning words (arXiv:2508.15842)
static HARMFUL_WORDS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    ["complexity", "guess", "stuck", "hard", "involved", "probably",
     "possibly", "likely", "perhaps", "maybe", "i think", "i believe",
     "depend", "miss", "beyond", "complex"].into_iter().collect()
});

// Sunk cost anchor phrases
static SUNK_COST_ANCHORS: LazyLock<Vec<&str>> = LazyLock::new(|| vec![
    "already built", "already implemented", "already written", "already invested",
    "we've spent", "can't throw away", "can't discard", "too much work",
    "not worth rewriting", "significant effort",
]);

// Sunk cost continuation phrases
static SUNK_COST_CONTINUATIONS: LazyLock<Vec<&str>> = LazyLock::new(|| vec![
    "so we should keep", "therefore we continue", "better to keep",
    "might as well", "instead of starting over", "instead of rewriting",
    "push through", "commit to", "worth continuing",
]);
```

## Implementation Details

### File Structure

```
src/analyzers/mod.rs        ← replace stub (traits, types, run_pipeline, catch_unwind)
src/analyzers/depth.rs       ← replace stub (DepthObserver)
src/analyzers/budget.rs      ← replace stub (BudgetObserver)
src/analyzers/bias.rs        ← replace stub (BiasObserver)
src/analyzers/confidence.rs  ← replace stub (ConfidenceEvaluator)
src/analyzers/sycophancy.rs  ← replace stub (SycophancyEvaluator)
src/thought.rs               ← add Clone to ThoughtResult/ThoughtRecord, clone branch records in Phase 1, call run_pipeline in Phase 2, populate WireResponse
Cargo.toml                   ← add rayon = "1.10"
```

### DepthObserver — Contradiction Detection (3 layers)

For each thought pair (current vs each previous on same branch) with `normalized_levenshtein > 0.7`:

```rust
fn detect_contradiction(thought_a: &str, thought_b: &str) -> bool {
    let a_lower = thought_a.to_lowercase();
    let b_lower = thought_b.to_lowercase();

    // Layer 1: Domain antonym pairs (includes negation-on-predicate)
    // This covers both "use sync" vs "use async" AND "should use X" vs "should not use X"
    // by treating (word, "not {word}") as implicit antonym pairs alongside explicit ones.
    let negations = ["not ", "no ", "never ", "cannot ", "shouldn't ", "won't ", "don't "];

    for (word_a, word_b) in ANTONYM_PAIRS.iter() {
        let a_has_first = a_lower.contains(word_a);
        let a_has_second = a_lower.contains(word_b);
        let b_has_first = b_lower.contains(word_a);
        let b_has_second = b_lower.contains(word_b);
        if (a_has_first && b_has_second) || (a_has_second && b_has_first) {
            return true;
        }

        // Negation-on-predicate: "use X" vs "not use X" (same word, one negated)
        if a_has_first && b_has_first {
            // Both mention the same word — check if one negates it
            let a_negated = negations.iter().any(|neg| {
                a_lower.find(neg).map_or(false, |pos| {
                    a_lower[pos..].contains(word_a)
                })
            });
            let b_negated = negations.iter().any(|neg| {
                b_lower.find(neg).map_or(false, |pos| {
                    b_lower[pos..].contains(word_a)
                })
            });
            if a_negated != b_negated {
                return true;
            }
        }
    }

    // Layer 2: Quantifier conflicts
    for (q_a, q_b) in QUANTIFIER_CONFLICTS.iter() {
        let a_has_first = a_lower.contains(q_a);
        let b_has_second = b_lower.contains(q_b);
        let a_has_second = a_lower.contains(q_b);
        let b_has_first = b_lower.contains(q_a);
        if (a_has_first && b_has_second) || (a_has_second && b_has_first) {
            return true;
        }
    }

    // Layer 3: Numeric mismatch — same entity with different numbers
    // (implementation: extract number-entity pairs, compare across thoughts)
    // Deferred to implementation — regex-based number extraction near named entities

    false
}
```

**Note**: Standalone negation count diff (the old Layer 1) is removed. Negation only fires when it flips a specific domain predicate — "should use async" vs "should not use async." This avoids false positives on refinements like "use PostgreSQL" → "use PostgreSQL, not Redis for caching" where the negation adds specificity rather than contradicting.
```

### ConfidenceEvaluator — 2x2 Hedging Matrix

```rust
fn substance_score(thought: &str, evidence_count: usize) -> u32 {
    let thought_lower = thought.to_lowercase();
    let harmful_count = HARMFUL_WORDS.iter()
        .filter(|w| thought_lower.contains(*w))
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

### BiasObserver — Sunk Cost Two-Keyword Detection

```rust
fn detect_sunk_cost(thought: &str, prev_thought: Option<&str>) -> bool {
    let lower = thought.to_lowercase();
    let has_anchor = SUNK_COST_ANCHORS.iter().any(|a| lower.contains(a));
    if !has_anchor { return false; }

    // Check continuation in same thought
    let has_continuation = SUNK_COST_CONTINUATIONS.iter().any(|c| lower.contains(c));
    if has_continuation {
        // False positive check: if question word present, it's a tradeoff discussion
        let question_words = ["should", "whether", "or ", " vs ", "instead"];
        let is_question = question_words.iter().any(|q| lower.contains(q));
        return !is_question;
    }

    // Check continuation in adjacent thought
    if let Some(prev) = prev_thought {
        let prev_lower = prev.to_lowercase();
        SUNK_COST_CONTINUATIONS.iter().any(|c| prev_lower.contains(c))
    } else {
        false
    }
}
```

### BiasObserver — Availability Detection (Entity Repetition)

```rust
fn detect_availability(records: &[ThoughtRecord], config: &Config) -> bool {
    if records.len() < 3 { return false; }
    if config.components.valid.is_empty() { return false; } // no components configured = no detection

    let known_entities: HashSet<&str> = config.components.valid.iter()
        .map(|s| s.as_str()).collect();

    // Count entity mentions across thoughts (one count per thought per entity)
    let mut entity_counts: HashMap<String, usize> = HashMap::new();
    for record in records {
        let words: HashSet<String> = record.input.thought.split_whitespace()
            .map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
            .collect();
        for word in &words {
            if known_entities.contains(word.as_str()) {
                *entity_counts.entry(word.clone()).or_insert(0) += 1;
            }
        }
    }

    let threshold = (records.len() as f64 * 0.4).ceil() as usize;
    entity_counts.values().any(|&count| count >= threshold)
}
```

### Integration — process_thought() Phase 1 + Phase 2

**Phase 1** (inside write lock): Clone branch-filtered records for Phase 2.

```rust
// Add Clone derive to ThoughtResult and ThoughtRecord
// In Phase 1, after branch filtering for recap:
let branch_records: Vec<ThoughtRecord> = trace.thoughts
    .iter()
    .filter(|t| t.input.branch_id == input.branch_id)
    .cloned()
    .collect();
```

Add `branch_records: Vec<ThoughtRecord>` to `TraceSnapshot`.

**Phase 2** (no lock): Call pipeline, populate WireResponse.

```rust
use crate::analyzers::run_pipeline;

let pipeline_result = run_pipeline(&input, &snapshot.branch_records, &self.config);

// Add panic warnings to the warnings vec
let mut warnings = generate_warnings(&input, &snapshot.recent_progress, &self.config);
warnings.extend(pipeline_result.panic_warnings);

Ok(WireResponse {
    // ... existing fields ...
    warnings,
    alerts: pipeline_result.alerts,
    confidence_reported: input.confidence,
    confidence_calculated: pipeline_result.confidence_calculated,
    confidence_gap: match (input.confidence, pipeline_result.confidence_calculated) {
        (Some(reported), Some(calculated)) => Some((reported - calculated).abs()),
        _ => None,
    },
    depth_overlap: pipeline_result.observations.prev_overlap,
    bias_detected: pipeline_result.observations.bias_detected,
    sycophancy: pipeline_result.sycophancy_pattern,
    // ... rest unchanged ...
})
```

### run_pipeline() — Catch Unwind Pattern

```rust
pub fn run_pipeline(
    input: &ThoughtInput,
    records: &[ThoughtRecord],
    config: &Config,
) -> PipelineResult {
    let mut panic_warnings = Vec::new();

    // Phase 1: Observers (named tuples for safe dispatch)
    let observers: Vec<(&str, Box<dyn Observer>)> = vec![
        ("depth", Box::new(DepthObserver)),
        ("budget", Box::new(BudgetObserver)),
        ("bias", Box::new(BiasObserver)),
    ];

    let observer_results: Vec<(&str, Result<Observation, _>)> = observers
        .par_iter()
        .map(|(name, obs)| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                obs.observe(input, records, config)
            }));
            (*name, result)
        })
        .collect();

    // Merge observations
    let mut observations = Observations::default();
    for (name, result) in observer_results {
        match result {
            Ok(obs) => observations.merge(obs),  // merge() also moves alerts from Observation::Depth into pending_alerts
            Err(_) => {
                panic_warnings.push(format!("WARNING [ANALYZER-PANIC]: {} observer panicked", name));
            }
        }
    }

    // Phase 2: Evaluators (named tuples)
    let evaluators: Vec<(&str, Box<dyn Evaluator>)> = vec![
        ("confidence", Box::new(ConfidenceEvaluator)),
        ("sycophancy", Box::new(SycophancyEvaluator)),
    ];

    let eval_results: Vec<(&str, Result<EvalOutput, _>)> = evaluators
        .par_iter()
        .map(|(name, eval)| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                confidence_calculated = Some(calculated);  // always set, even when no alert
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

    // Drain depth observer alerts (PREMATURE_TOPIC_SWITCH, UNRESOLVED_CONTRADICTION, SHALLOW_ANALYSIS)
    alerts.extend(observations.drain_alerts());

    PipelineResult { alerts, observations, confidence_calculated, sycophancy_pattern, panic_warnings }
}
```

### Depth Observer Alerts

The depth observer produces both observations (data for evaluators) AND alerts (for Claude). The alerts are:

| Alert | Condition | Severity |
|-------|-----------|----------|
| `PREMATURE_TOPIC_SWITCH` | overlap < 0.3 with previous thought on same branch | Medium |
| `UNRESOLVED_CONTRADICTION` | any of 3 heuristic layers fires between non-revision thought pairs | High |
| `SHALLOW_ANALYSIS` | >50% of branch thoughts have `normalized_levenshtein` >0.7 with predecessor | Medium |

These are stored in `Observations` and drained into `alerts` after the merge.

## Test Strategy

Tests inline in each analyzer file `#[cfg(test)]`.

**mod.rs**: `test_pipeline_runs`, `test_observer_panic_produces_warning`, `test_evaluator_panic_produces_warning`, `test_observations_merge`, `test_observations_drain_alerts`, `test_confidence_always_returns_score`

**depth.rs**: `test_prev_overlap_high_similarity`, `test_prev_overlap_low_similarity`, `test_initial_overlap_computed`, `test_initial_overlap_none_on_first_thought`, `test_topic_switch_fires`, `test_antonym_contradiction`, `test_negation_on_predicate_contradiction`, `test_quantifier_conflict`, `test_no_contradiction_different_topics`, `test_refinement_not_contradiction` (adding specificity with negation), `test_shallow_analysis_fires`, `test_revision_excluded_from_contradiction`, `test_depth_alerts_in_observation`

**budget.rs**: `test_budget_observation_architecture`, `test_budget_observation_implementation`, `test_budget_unknown_mode`, `test_budget_no_alerts`

**bias.rs**: `test_anchoring_detected`, `test_confirmation_bias_detected`, `test_sunk_cost_detected`, `test_sunk_cost_false_positive_question`, `test_availability_detected`, `test_availability_no_components_no_detection`, `test_overconfidence_timing`, `test_no_bias_clean_reasoning`, `test_first_match_wins`

**confidence.rs**: `test_high_evidence_high_score`, `test_no_evidence_low_score`, `test_overconfidence_alert_fires`, `test_overconfidence_within_gap_no_alert_but_score_returned`, `test_substance_hedging_no_evidence`, `test_substance_hedging_with_evidence`, `test_substance_no_hedging_with_evidence`, `test_substance_no_hedging_no_evidence`, `test_contradiction_penalty`, `test_branch_alternatives_bonus`

**sycophancy.rs**: `test_premature_agreement_fires`, `test_premature_agreement_with_challenge_ok`, `test_no_self_challenge_fires`, `test_no_self_challenge_with_revision_ok`, `test_confirmation_only_fires_initial_overlap`, `test_confirmation_only_with_counter_argument_ok`, `test_confirmation_only_with_branch_ok`, `test_confirmation_only_uses_initial_not_prev_overlap`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| strsim thresholds produce false positives/negatives | Medium | Medium | Research-backed thresholds (0.3/0.7). Tunable via config later. | Test with real thought examples |
| Rayon thread pool startup stall on first call | Low | Low | One-time ~1ms cost, acceptable | Manual timing |
| Antonym pairs miss domain-specific contradictions | Medium | Low | Start with 22 pairs, extensible. Trace review catches rest. | Add pairs as discovered |
| Counter-argument keywords are unvalidated | Medium | Medium | Acknowledged in brief. Best available heuristic. | Track false positive rate in production |
| catch_unwind + rayon interaction | Low | High | AssertUnwindSafe wrapper. Rayon docs confirm this pattern works. | test_observer_panic_produces_warning |

## Success Criteria

- [ ] `run_pipeline()` produces correct alerts for all 5 analyzer types
- [ ] Depth observer computes both `prev_overlap` and `initial_overlap`
- [ ] Depth observer catches antonym pairs, negation-on-predicate, quantifier conflicts (no standalone negation count)
- [ ] Depth observer produces alerts via `pending_alerts` in Observations
- [ ] Confidence evaluator always returns calculated score (even when no overconfidence alert) via `EvalOutput::Confidence`
- [ ] Confidence evaluator uses 2x2 hedging×evidence matrix with `normalized_levenshtein`
- [ ] Sycophancy evaluator uses `initial_overlap` (not `prev_overlap`) for CONFIRMATION_ONLY
- [ ] Sycophancy evaluator checks counter-argument keywords in intermediate thoughts
- [ ] Bias observer catches 5 biases with sunk cost two-keyword requirement
- [ ] Availability detection returns false when `components.valid` is empty (no fallback)
- [ ] Budget observer returns observation only (no alerts)
- [ ] Observer/Evaluator traits have `Send + Sync` bounds
- [ ] Named tuple dispatch (no index-based name array)
- [ ] `catch_unwind` per analyzer — panic produces warning, not crash
- [ ] All ~55 new tests pass + all ~106 existing tests pass
- [ ] `cargo check` passes
