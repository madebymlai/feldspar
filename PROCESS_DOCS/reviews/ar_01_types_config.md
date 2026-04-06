# Adversarial Review: Types + Config (01-types-config.md)

## Summary

Design is sound for types and config structure. One critical gap: principles YAML deserialization is structurally impossible as specified. Four recommended improvements are zero-cost derive additions and validation hardening. After amending the principles loading path, the design is ready for `/breakdown`.

**Reviewers**: ar-o (Opus), ar-k (Kimi/Valence), ar-glm5 (GLM-5.1)

## Critical (Must Address)

### Principles YAML deserialization is structurally impossible
**Flagged by**: ar-o, ar-k, ar-glm5 (3/3)  |  **Confidence**: High

The YAML file has `groups:` as a top-level map with group names as keys (`solid:`, `kiss-dry:`, etc.). The design's `PrincipleGroup { name, active, principles }` cannot be directly deserialized from this structure — serde has no mechanism to promote a map key into a struct field. The comment "group key becomes name" acknowledges the problem but provides no mechanism. Additionally, `load_principles()` is referenced in `Config::load()` but never defined.

| Factor | Assessment |
|--------|------------|
| Severity | System failure — config loading will crash at runtime |
| Probability | Guaranteed — structurally impossible as designed |
| Remediation Cost | Simple fix — add RawPrinciples wrapper type + mapping function |
| Reversibility | Fixable now, no reason to defer |
| Context Fit | Core config functionality, blocks any principles-related testing |

**Mitigation**: Add two-stage deserialization:
```rust
#[derive(Deserialize)]
struct RawPrinciples {
    groups: HashMap<String, RawPrincipleGroup>,
}

#[derive(Deserialize)]
struct RawPrincipleGroup {
    #[serde(default)]
    active: bool,
    principles: Vec<Principle>,
}
```
`load_principles()` deserializes into `RawPrinciples`, iterates the HashMap, filters `active == true`, validates non-empty principles, and maps `(key, data) → PrincipleGroup { name: key, active: data.active, principles: data.principles }`.

## Recommended (High Value)

### Add Deserialize to ThoughtResult, Alert, Severity
**Flagged by**: ar-o  |  **Confidence**: High

The DB schema stores `analyzer_output TEXT -- JSON blob` and `warnings TEXT -- JSON array`. `load_history()` (confirmed in `src/db.rs`) reads these back for ML bulk training. Without `Deserialize` on ThoughtResult/Alert/Severity, issue #5 (DB) will be blocked or forced to define duplicate types.

| Factor | Assessment |
|--------|------------|
| Severity | Degraded UX — blocks issue #5 or forces type duplication |
| Probability | Guaranteed — `load_history()` is a defined operation |
| Remediation Cost | Simple fix — add `Deserialize` derive (zero runtime cost) |
| Reversibility | Fixable later but costs nothing now |
| Context Fit | Prevents downstream issue from modifying this issue's types |

**Mitigation**: Add `Deserialize` to `ThoughtResult`, `Alert`, `Severity`. The brief's input/output separation (ThoughtInput = Deserialize-only) still holds — ThoughtResult gets both because it's stored and read back.

### Add PartialEq/Eq to Severity, Clone to ThoughtInput
**Flagged by**: ar-o, ar-glm5  |  **Confidence**: High

Analyzers will pattern-match on Severity values (needs PartialEq). Pattern recall and logging may clone ThoughtInput. All fields are owned types, so Clone is free.

| Factor | Assessment |
|--------|------------|
| Severity | Minor inconvenience — downstream issues would add the derives |
| Probability | Guaranteed — analyzer pipeline matches on severity |
| Remediation Cost | Zero — free derives |
| Reversibility | Trivially fixable later |
| Context Fit | Prevents churn across multiple downstream issues |

**Mitigation**: Add `#[derive(PartialEq, Eq)]` to `Severity`. Add `#[derive(Clone)]` to `ThoughtInput`, `ThoughtRecord`, `Impact`, `Alert`.

### Validate mode.requires against known set
**Flagged by**: ar-o, ar-glm5  |  **Confidence**: Medium

`requires = ["components"]` maps to ThoughtInput fields, but a typo like `requires = ["componets"]` passes validation silently. The warning engine (later issue) would never trigger for the misspelled requirement.

| Factor | Assessment |
|--------|------------|
| Severity | Degraded UX — silent config typos |
| Probability | Common path — users edit mode configs |
| Remediation Cost | Simple fix — hardcode valid set in validation |
| Reversibility | Fixable later |
| Context Fit | Config validation is explicitly this issue's scope |

**Mitigation**: Add validation: each value in `mode.requires` must be one of `["components", "evidence", "latency", "confidence"]`. Panic with clear message on unknown value.

### Document response assembly strategy
**Flagged by**: ar-o  |  **Confidence**: Medium

ThoughtResult field names (`ml_trajectory`, `ml_drift`) don't match the upstream wire format (`trajectory`, `driftDetected`). Echo-back fields (`traceId`, `thoughtNumber`, `branches`) aren't in ThoughtResult at all. No documented strategy for how issue #3 builds the flat response.

| Factor | Assessment |
|--------|------------|
| Severity | Minor inconvenience — implementer has to figure it out |
| Probability | Guaranteed — formats visibly diverge |
| Remediation Cost | Simple fix — add note to Integration Points section |
| Reversibility | Documentation only, fixable anytime |
| Context Fit | Useful for issue #3 implementer |

**Mitigation**: Add to Integration Points: "Issue #3 (MCP server) builds the flat wire response by merging: echo-back fields from ThoughtInput (thoughtNumber, totalThoughts, nextThoughtNeeded), Trace metadata (traceId, branches, thoughtHistoryLength), and ThoughtResult fields (renamed where wire format differs: ml_trajectory → trajectory, ml_drift → driftDetected). ThoughtResult is the internal representation; the wire format is a superset assembled at the handler level."

## Noted (Awareness)

- **Collect all validation errors before panicking**: DX improvement — show all config errors at once instead of fix-one-retry. Low priority.
- **`budget_used`/`budget_max` default to 0**: Edge case if ThoughtResult constructed without budget analyzer. Issue #3's concern — budget analyzer always runs on the happy path.
- **`confidence` range unvalidated at type level**: Valid concern but belongs in thought processing (issue #3), not the type layer. Document valid range (0-100) as a comment.
- **Budget category stays String, not enum**: Budget tiers are configurable per project. An enum would lock the set at compile time, contradicting "New mode = add a block to config. No recompile."
- **DIP traits for db/ml**: CLAUDE.md says DIP, but empty trait objects for zero-behavior markers are ceremony. Issues #5/#6 will touch ThinkingServer regardless. KISS wins.
- **RwLock write contention**: Trace-level locking is an issue #3 concern. The RwLock wraps the outer HashMap (trace lookup), not per-thought processing.
- **`watches` field role**: Clarify when warning engine is designed (issue #4). For now it's a documentation string.
- **Rust edition 2024 requires 1.85+**: Note in quickstart/install docs.
- **Trace.id as uuid::Uuid vs String**: uuid::Uuid is cleaner but String works. Low priority.
- **ThinkingServer field visibility**: Consider `pub(crate)` when methods are added in issue #3.
- **Send+Sync bounds for future db/ml types**: Document as constraint for issues #5/#6.

## Recommendation

[x] REVISE — One Critical issue (principles YAML deserialization) requires a design amendment before `/breakdown`
[ ] PROCEED
