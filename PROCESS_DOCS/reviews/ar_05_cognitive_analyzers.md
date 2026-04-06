# Adversarial Review: Cognitive Analyzers — Observer/Evaluator Pipeline (05-cognitive-analyzers.md)

## Summary

Design is architecturally sound with strong research backing (9 papers), but has 6 critical implementation gaps: wrong overlap signal for sycophancy, undefined drain_alerts(), missing Send+Sync bounds, incomplete confidence extraction, negation flip false positives, and unspecified strsim function. All are simple fixes. After amendments, ready for `/breakdown`.

**Reviewers**: ar-o (Opus), ar-k (Kimi/Valence), ar-glm (GLM-5.1)

## Critical (Must Address)

### `depth_overlap` delivers wrong signal to sycophancy evaluator
**Flagged by**: ar-o, ar-k (2/3)  |  **Confidence**: High

`Observation::Depth.overlap` is "strsim with previous thought" but sycophancy's CONFIRMATION_ONLY needs "strsim between thought 1 and final thought." Evaluator reads prev-vs-current, needs initial-vs-current.

| Factor | Assessment |
|--------|------------|
| Severity | Highest-severity sycophancy pattern fires on wrong signal |
| Probability | Guaranteed |
| Remediation Cost | Simple — add `initial_overlap` field |
| Reversibility | Must fix now — shapes observer/evaluator contract |
| Context Fit | CONFIRMATION_ONLY is "the most dangerous pattern" per the design |

**Mitigation**: Depth observer computes two values: `prev_overlap` (current vs predecessor, for topic switch/shallow) and `initial_overlap` (current vs thought 1 on branch, for sycophancy). Add both to `Observation::Depth` and `Observations`.

### `observations.drain_alerts()` called but never defined
**Flagged by**: ar-o, ar-glm, ar-k (3/3)  |  **Confidence**: High

`Observations` struct has no alerts buffer and no `drain_alerts()` method. Depth observer produces both observations AND alerts with no storage mechanism.

| Factor | Assessment |
|--------|------------|
| Severity | Won't compile |
| Probability | Guaranteed |
| Remediation Cost | Simple — add `pending_alerts: Vec<Alert>` to Observations |
| Reversibility | Trivial |
| Context Fit | Observer returning both data and alerts needs explicit storage |

**Mitigation**: Add `pending_alerts: Vec<Alert>` to `Observations`. The `merge()` method for `Observation::Depth` pushes alerts into this buffer. Implement `drain_alerts(&mut self) -> Vec<Alert>`.

### Observer/Evaluator traits need `Send + Sync` bounds
**Flagged by**: ar-o, ar-glm, ar-k (3/3)  |  **Confidence**: High

`par_iter()` on `&[&dyn Observer]` requires `Observer: Send + Sync`. Without these bounds, rayon won't compile.

| Factor | Assessment |
|--------|------------|
| Severity | Won't compile |
| Probability | Guaranteed |
| Remediation Cost | Trivial — one line per trait |
| Reversibility | Trivial |
| Context Fit | All analyzer structs are stateless, auto-implement Send+Sync |

**Mitigation**: `pub trait Observer: Send + Sync` and `pub trait Evaluator: Send + Sync`.

### `confidence_calculated` silently dropped when no overconfidence alert
**Flagged by**: ar-o, ar-glm, ar-k (3/3)  |  **Confidence**: High

`ConfidenceEvaluator` returns `Option<Alert>`. When there's no overconfidence (gap < 25), it returns `None`, but the calculated score should always be returned so Claude sees it.

| Factor | Assessment |
|--------|------------|
| Severity | Calculated confidence invisible on clean thoughts |
| Probability | Guaranteed — most thoughts won't trigger overconfidence |
| Remediation Cost | Simple — richer return type |
| Reversibility | Shapes the Evaluator trait contract |
| Context Fit | Claude seeing the calculated score is the main value of the confidence calibrator |

**Mitigation**: `ConfidenceEvaluator` returns a custom struct:
```rust
pub struct ConfidenceResult {
    pub calculated: f64,
    pub alert: Option<Alert>,
}
```
`run_pipeline` extracts both. The `Evaluator` trait can return `EvalOutput` enum that wraps either `Option<Alert>` (for sycophancy) or `ConfidenceResult` (for confidence). Alternatively, store `confidence_calculated` in `PipelineResult` directly and have the evaluator write to a shared output struct.

### Negation flip contradiction detection will false-positive on refinements
**Flagged by**: ar-o (1/3)  |  **Confidence**: High

"We should use PostgreSQL" → "We should use PostgreSQL, not Redis" adds one negation to a similar thought. Layer 1 fires. Normal reasoning adds specificity through negation.

| Factor | Assessment |
|--------|------------|
| Severity | 1-3 false contradictions per trace erodes trust in all alerts |
| Probability | High — refinement is normal reasoning |
| Remediation Cost | Simple — merge Layer 1 into Layer 2 |
| Reversibility | Fixable later but false positives are worse than missing contradictions |
| Context Fit | Research (Stanford ACL 2008) says antonym alignment is the reliable cue, not raw negation count |

**Mitigation**: Remove standalone negation count diff (Layer 1). Instead, negation only fires when it flips a domain predicate — merge into Layer 2: "should use async" vs "should not use async" catches the negation ON the antonym pair. Negation without a specific flipped predicate is ignored.

### Unspecified strsim function — thresholds meaningless
**Flagged by**: ar-o (1/3)  |  **Confidence**: High

`strsim` crate has `normalized_levenshtein`, `jaro_winkler`, `sorensen_dice`, etc. Each produces different score distributions. 0.7 on `jaro_winkler` catches far more pairs than 0.7 on `normalized_levenshtein`.

| Factor | Assessment |
|--------|------------|
| Severity | Builder picks wrong function, all thresholds become meaningless |
| Probability | Guaranteed — design says "strsim" 15 times, never specifies which |
| Remediation Cost | Trivial — one line in the design |
| Reversibility | Trivial |
| Context Fit | `normalized_levenshtein` is the conservative choice with well-understood distribution |

**Mitigation**: Specify `strsim::normalized_levenshtein` throughout. Document this choice in the design.

## Recommended (High Value)

### Index-based name dispatch → named tuples
**Flagged by**: ar-glm (1/3)  |  **Confidence**: Medium

`["depth", "budget", "bias"][i]` breaks silently if observers reordered.

**Mitigation**: Use `vec![("depth", &depth_obs as &dyn Observer), ...]` and destructure.

### Acknowledge hardcoded thresholds honestly
**Flagged by**: ar-o, ar-k (2/3)  |  **Confidence**: Medium

Design claims "all thresholds from config" but most are hardcoded. Not wrong to hardcode — wrong to claim otherwise.

**Mitigation**: Document as structural constants. Only `confidence_gap` comes from config. Others are tunable in code, not config. Remove the false claim.

### Shallow analysis: add new-token check
**Flagged by**: ar-o (1/3)  |  **Confidence**: Medium

Without "no new entities/numbers" verification, incremental reasoning false-positives on SHALLOW_ANALYSIS.

**Mitigation**: Count unique tokens in current thought not present in predecessor. If >3 new tokens, it's building-on, not rephrasing. Skip the shallow flag for that thought.

### Availability: fall back to word frequency when no components configured
**Flagged by**: ar-o (1/3)  |  **Confidence**: Medium

`components.valid = []` silently disables availability bias detection.

**Mitigation**: When `components.valid` is empty, extract multi-occurrence nouns (split on whitespace, filter stop words, track terms appearing in >40% of thoughts). Less precise but better than silent failure.

## Noted (Awareness)

- **BudgetObserver justification**: Consumed by confidence evaluator (alternatives via branches) and sunk cost bias detection. Not dead weight.
- **Full ThoughtRecord clone vs lightweight extraction**: Acceptable for v1 with <20 thoughts per trace. Optimize later if traces grow. Create `AnalyzerRecord` if profiling shows allocation pressure.
- **HashSet for substring search**: Cosmetic — `Vec<&str>` would be more honest about the access pattern. No performance impact at this scale.
- **Counter-argument keywords unvalidated**: Already acknowledged in brief. Flag as "v0.1 heuristic — expect iteration."
- **Rayon overhead**: User chose this explicitly. Thread pool startup ~1ms one-time. Acceptable foundation for future growth.
- **strsim thread safety**: False positive from ar-k. Pure functions are inherently thread-safe.

## Recommendation

[x] REVISE — 6 Critical issues require design amendments before `/breakdown`
[ ] PROCEED
