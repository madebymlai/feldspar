# Adversarial Review: Warning Engine — Regex Patterns, Budget Checks, Mode Validation (04-warning-engine.md)

## Summary

Design is solid but has 5 critical issues: missing `regex` dep, budget ownership conflict with analyzers, silent budget fallback, Phase 1 data flow gap (including missing `Clone` derives), and broken `has_recent_progress()` logic for branches. All are simple fixes. After amendments, ready for `/breakdown`.

**Reviewers**: ar-o (Opus), ar-k (Kimi/Valence), ar-glm (GLM-5.1)

## Critical (Must Address)

### `regex` crate missing from Cargo.toml
**Flagged by**: ar-o, ar-k (2/3)  |  **Confidence**: High

Design states "regex crate (already in Cargo.toml)" — verified false. `strsim` is present, `regex` is not. Won't compile.

| Factor | Assessment |
|--------|------------|
| Severity | Build failure — module won't compile |
| Probability | Guaranteed |
| Remediation Cost | Trivial — one line |
| Reversibility | Trivial |
| Context Fit | Must-have dependency for the entire module |

**Mitigation**: Add `regex = "1"` to `[dependencies]` in `Cargo.toml`.

### Budget warning / analyzer ownership conflict
**Flagged by**: ar-o, ar-k, ar-glm (3/3)  |  **Confidence**: High

`src/analyzers/budget.rs` (issue #5) defines UNDERTHINKING and OVERTHINKING alerts via the Observer trait. The warning engine also implements UNDERTHINKING, OVERTHINKING, and OVER-ANALYSIS. When both ship, Claude sees duplicate pushback in different formats (`alerts[]` and `warnings[]`).

| Factor | Assessment |
|--------|------------|
| Severity | Duplicate/contradictory pushback to Claude |
| Probability | Guaranteed when both issues ship |
| Remediation Cost | Simple — decide ownership now |
| Reversibility | Load-bearing decision — shapes both modules |
| Context Fit | Core design boundary between warnings and analyzers |

**Mitigation**: Warning engine owns all budget checks (OVER-ANALYSIS, OVERTHINKING, UNDERTHINKING). Budget analyzer becomes observation-only — emits `Observation::Budget { used, max, category }` for evaluators to consume, but no alerts. This is cleaner: warnings = simple threshold checks, analyzers = cross-thought pattern analysis.

### `resolve_budget()` silent fallback masks config errors
**Flagged by**: ar-o, ar-glm (2/3)  |  **Confidence**: High

Default return `(3, 5, "standard")` fires when mode is unknown or missing. A typo in `thinking_mode` silently gets standard budget instead of an error. The "standard" tier isn't guaranteed to exist in config.

| Factor | Assessment |
|--------|------------|
| Severity | Incorrect budget values, masked config errors |
| Probability | Common — Claude sends freeform mode strings |
| Remediation Cost | Simple — return `Option`, add warning |
| Reversibility | Fixable later but wrong values are worse than no values |
| Context Fit | Unknown mode producing wrong UNDERTHINKING alerts is real |

**Mitigation**: `resolve_budget()` returns `Option<(u32, u32, String)>`. When `None`, the warning engine fires `"WARNING [UNKNOWN-MODE]: thinking_mode '{mode}' not found in config. No budget checks applied."`. `process_thought()` falls back to `(thought_number, 5, "standard")` only when no mode provided (legitimate default for untyped thoughts).

### Phase 1 data flow + `ThoughtRecord` not `Clone`
**Flagged by**: ar-o, ar-k, ar-glm (3/3)  |  **Confidence**: High

`TraceSnapshot` doesn't carry records. `check_budget()` needs last 3 records for the overthinking check. `ThoughtRecord` and `ThoughtResult` don't derive `Clone`, so they can't be added to `TraceSnapshot`.

| Factor | Assessment |
|--------|------------|
| Severity | Can't implement overthinking check |
| Probability | Guaranteed |
| Remediation Cost | Simple — add `Clone` derives + lightweight extraction |
| Reversibility | Architectural — Phase 1/2 boundary |
| Context Fit | Directly blocks implementation |

**Mitigation**: Add `Clone` to `ThoughtResult` and `ThoughtRecord`. In Phase 1, extract a lightweight struct for the last 3 branch-filtered records: `Vec<(bool, Option<String>)>` (is_revision, branch_from_thought). Add to `TraceSnapshot`. No need to clone full records — only the two fields `check_budget()` inspects.

### `has_recent_progress()` broken for branch-filtered records
**Flagged by**: ar-o (1/3)  |  **Confidence**: High

The function checks `r.input.branch_id.is_some()`. If records are branch-filtered, non-main-line records always have `branch_id.is_some() == true`, making `has_recent_progress()` always return true and suppressing OVERTHINKING on all branches.

| Factor | Assessment |
|--------|------------|
| Severity | OVERTHINKING never fires on branches |
| Probability | Common — branching is a core feature |
| Remediation Cost | Simple — fix the check |
| Reversibility | Trivial |
| Context Fit | Defeats the purpose of the overthinking guard on branches |

**Mitigation**: Check `is_revision` or `branch_from_thought.is_some()` (indicates a *new* branch was created from this thought). Not `branch_id.is_some()` which just means "this thought is on a branch."

## Recommended (High Value)

### Within-label dedup per `generate_warnings()` call
**Flagged by**: ar-glm (1/3)  |  **Confidence**: Medium

"let's just do a quick hack" matches two ANTI-QUICK-FIX patterns, producing two identical warnings for the same thought.

| Factor | Assessment |
|--------|------------|
| Severity | Minor noise |
| Probability | Occasional |
| Remediation Cost | Trivial — dedup by label before returning |
| Reversibility | Trivial |
| Context Fit | Easy win, no downside |

**Mitigation**: After collecting all warnings, dedup by label within a single `generate_warnings()` call. Keep first occurrence per label.

### Graduate budget warnings — OVER-ANALYSIS → OVERTHINKING only
**Flagged by**: ar-o, ar-k (2/3)  |  **Confidence**: Medium

Past 2x budget, Claude gets both OVER-ANALYSIS and OVERTHINKING every thought. On a 25-thought trace, ~36 identical budget warnings.

| Factor | Assessment |
|--------|------------|
| Severity | Noise — warnings lose impact |
| Probability | Occasional but bad when it happens |
| Remediation Cost | Simple — suppress OVER-ANALYSIS when OVERTHINKING fires |
| Reversibility | Trivial |
| Context Fit | Reddit post targets 5-8 thought traces. Feldspar traces may be longer. |

**Mitigation**: Fire OVER-ANALYSIS at 1.5x threshold. Once past 2.0x, fire only OVERTHINKING (suppress OVER-ANALYSIS). Each fires once at its threshold, not every subsequent thought.

### Document `warnings` vs `alerts` field ownership
**Flagged by**: ar-glm, ar-k (2/3)  |  **Confidence**: Medium

`WireResponse` has `warnings: Vec<String>` and `alerts: Vec<Alert>`. The design's commented-out code merges alerts into warnings. This contradicts the two-field design.

| Factor | Assessment |
|--------|------------|
| Severity | Architectural confusion when issue #5 ships |
| Probability | Guaranteed |
| Remediation Cost | Trivial — document the boundary |
| Reversibility | Fixable later |
| Context Fit | Sets expectations for issue #5 builder |

**Mitigation**: Document: `warnings` = flat string warnings from the warning engine. `alerts` = structured `Alert` objects from the analyzer pipeline. No merging — Claude sees both fields. Remove the commented-out merge code from the design.

## Noted (Awareness)

- **Regex false positives**: "simply use the existing trait" triggers ANTI-QUICK-FIX. Accepted — warnings are advisory, false positives are tolerable. Add `\b` at end of verb patterns to reduce. Add a false-positive test case.
- **Float cast**: `thought_number as f64 > total_thoughts as f64 * multiplier`. Trivial implementation detail — document in code sketch.
- **Dead code paths**: `NO-LATENCY` and `NO-CONFIDENCE` have no current mode triggers. Fine for extensibility. Add tests with custom config to cover.
- **`LazyLock` vs `once_cell`**: ar-k flagged, ar-o validated. `LazyLock` is correct for Rust 1.92. No issue.
- **First-call regex compilation latency**: ~100μs one-time stall in async context. Acceptable.

## Recommendation

[x] REVISE — 5 Critical issues require design amendments before `/breakdown`
[ ] PROCEED
