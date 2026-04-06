# Solution Design: Warning Engine

## Executive Summary

Advisory warning system that pushes back on Claude when it cuts corners. Three independent checkers — language patterns, budget tracking, mode validation — produce flat warning strings injected into the tool response. Claude reads them and self-corrects. No blocking, no gating. Pure sync, microsecond-scale.

## Rationale

| Decision | Rationale | Alternative | Why Rejected |
|----------|-----------|-------------|--------------|
| `LazyLock` for regex | std since Rust 1.80, no external dep | `lazy_static` crate | Extra dep for same functionality |
| Flat `Vec<String>` output | Matches existing `WireResponse.warnings` field | Structured warning types | Adds complexity, Claude reads strings not structs |
| Shared `resolve_budget()` helper | Budget logic needed by both warnings and `process_thought()` | Duplicate the logic | DRY — same mode→tier→[min,max] lookup |
| `watches` as documentation only | Qualitative checks need NLP/LLM to enforce | Regex on watches text | False positives, brittle, low value |
| Within-label dedup per call | "just do a quick hack" matches two ANTI-QUICK-FIX patterns — fire once | No dedup | Duplicate warnings for same thought is noise |
| Warning engine owns budget checks | Clear boundary: warnings = threshold checks, analyzers = cross-thought patterns | Budget analyzer also emits alerts | Duplicate pushback in `warnings[]` and `alerts[]` |
| `warnings` and `alerts` are separate | `warnings` = flat strings from engine, `alerts` = structured from analyzers | Merge alerts into warnings | Two-field design already exists in WireResponse |
| Budget warnings graduate | OVER-ANALYSIS at 1.5x, OVERTHINKING at 2.0x (suppresses OVER-ANALYSIS) | Fire both every thought | 36+ identical warnings on long traces is noise |

## Technology Stack

- `regex` crate — **new dependency**, add `regex = "1"` to `Cargo.toml`
- `std::sync::LazyLock` (Rust 1.80+, no dep)

## Architecture

### Data Flow

```
ThoughtInput + [ThoughtRecord] + Config
    │
    ├── check_language(&input.thought) → Vec<String>
    ├── check_budget(&input, records, &config) → Vec<String>
    └── check_mode(&input, &config) → Vec<String>
    │
    └── merged into Vec<String> → returned to process_thought()
                                      │
                                      └── into WireResponse.warnings (analyzer alerts go into WireResponse.alerts separately)
```

### Component Catalog

| Component | File | Purpose |
|-----------|------|---------|
| `generate_warnings()` | `src/warnings.rs` | Entry point — calls all 3 checkers, merges results |
| `check_language()` | `src/warnings.rs` | Regex matching against thought text |
| `check_budget()` | `src/warnings.rs` | Budget threshold checks (over-analysis, overthinking, underthinking) |
| `check_mode()` | `src/warnings.rs` | Mode-specific field presence validation |
| `resolve_budget()` | `src/config.rs` | Shared helper — mode → tier → [min, max] lookup |

### Integration Point

In `src/thought.rs` `process_thought()` Phase 2 (after write lock drops):

```rust
// Generate warnings (warning engine — flat strings)
let warnings = generate_warnings(&input, &snapshot.recent_progress, &self.config);

// Populate WireResponse — warnings and alerts are separate fields
wire.warnings = warnings;
// wire.alerts populated by analyzer pipeline (issue #5) — NOT merged into warnings
```

**Field ownership**: `WireResponse.warnings` = flat string warnings from the warning engine. `WireResponse.alerts` = structured `Alert` objects from the analyzer pipeline (issue #5). Claude sees both. No merging.

## Protocol/Schema

### Warning Format

All warnings follow: `"WARNING [LABEL]: message"`

### Language Warnings

| Label | Regex | Message |
|-------|-------|---------|
| ANTI-QUICK-FIX | `\b(just\|simply)\s+(do\|use\|add\|skip\|ignore\|throw\|hack\|slap)\b` | Shortcut language detected — justify this approach or propose a proper solution. |
| ANTI-QUICK-FIX | `\bquick\s*(fix\|solution\|hack)\b` | Shortcut language detected — justify this approach or propose a proper solution. |
| ANTI-QUICK-FIX | `\bgood\s+enough\b` | Shortcut language detected — justify this approach or propose a proper solution. |
| ANTI-QUICK-FIX | `\bshould\s+be\s+fine\b` | Shortcut language detected — justify this approach or propose a proper solution. |
| DISMISSAL | `\bpre.?existing\s+(issue\|problem\|bug)` | Dismissal language detected — address the issue or explain why it's out of scope. |
| DISMISSAL | `\bout\s+of\s+scope` | Dismissal language detected — address the issue or explain why it's out of scope. |
| DISMISSAL | `\bnot\s+(my\|our)\s+(problem\|concern)` | Dismissal language detected — address the issue or explain why it's out of scope. |
| DISMISSAL | `\b(already\|was)\s+broken` | Dismissal language detected — address the issue or explain why it's out of scope. |
| DISMISSAL | `\bworked\s+before` | Dismissal language detected — address the issue or explain why it's out of scope. |
| DISMISSAL | `\bknown\s+issue` | Dismissal language detected — address the issue or explain why it's out of scope. |

All regex case-insensitive.

### Budget Warnings

| Label | Condition | Message |
|-------|-----------|---------|
| OVER-ANALYSIS | `thought_number > total_thoughts * over_analysis_multiplier` | At thought {n} of estimated {total}. Conclude or justify continuing. |
| OVERTHINKING | `thought_number > total_thoughts * overthinking_multiplier` AND no branches/revisions in last 3 records | Past {multiplier}x your estimate with no new insights. Make a decision or branch. |
| UNDERTHINKING | `!next_thought_needed` AND `thought_number < budget_min` | Wrapping up a {mode} analysis in {n} thoughts when minimum is {min}. This needs more depth. |

### Mode Warnings

| Requires value | Check | Label | Message |
|----------------|-------|-------|---------|
| `"evidence"` | `input.evidence.is_empty()` | NO-EVIDENCE | {mode} mode requires citations — file paths, logs, stack traces. |
| `"components"` | `input.affected_components.is_empty()` | NO-COMPONENTS | {mode} mode requires naming affected components. |
| `"latency"` | `input.estimated_impact` missing or `.latency.is_none()` | NO-LATENCY | {mode} mode requires latency estimates. |
| `"confidence"` | `input.confidence.is_none()` | NO-CONFIDENCE | {mode} mode requires a confidence rating. |

## Implementation Details

### File Structure

```
src/warnings.rs    ← main implementation (replace stub)
src/config.rs      ← add resolve_budget() helper
src/thought.rs     ← call generate_warnings() in Phase 2
```

### Regex Compilation (LazyLock)

```rust
use std::sync::LazyLock;
use regex::Regex;

struct WarningPattern {
    regex: Regex,
    label: &'static str,
    message: &'static str,
}

static LANGUAGE_PATTERNS: LazyLock<Vec<WarningPattern>> = LazyLock::new(|| {
    vec![
        WarningPattern {
            regex: Regex::new(r"(?i)\b(just|simply)\s+(do|use|add|skip|ignore|throw|hack|slap)").unwrap(),
            label: "ANTI-QUICK-FIX",
            message: "Shortcut language detected — justify this approach or propose a proper solution.",
        },
        // ... rest of patterns
    ]
});
```

### resolve_budget() Helper (in config.rs)

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

This replaces the inline budget resolution in `process_thought()` and is reused by `check_budget()`.

**Caller behavior**:
- `process_thought()`: when `None`, falls back to `(thought_number, 5, "standard")` — legitimate default for untyped thoughts.
- `check_budget()`: when `None` and `thinking_mode` was `Some(name)`, fires `"WARNING [UNKNOWN-MODE]: thinking_mode '{name}' not found in config. No budget checks applied."`. When `thinking_mode` is `None`, no budget warnings (no mode = no budget expectations).

### check_budget() — Overthinking Detail

The "no new branches/revisions in last 3" check operates on `RecentProgress` tuples extracted in Phase 1:

```rust
/// (is_revision, branch_from_thought) — lightweight extract from ThoughtRecord
pub type RecentProgress = Vec<(bool, Option<u32>)>;

fn has_recent_progress(recent: &RecentProgress) -> bool {
    recent.iter().any(|(is_revision, branch_from)| *is_revision || branch_from.is_some())
}
```

`branch_from_thought.is_some()` means "a new branch was *created* from this thought" — genuine progress. This replaces the broken `branch_id.is_some()` check which was always true for non-main-line branches.

**Budget warning graduation**: OVER-ANALYSIS fires at 1.5x threshold. Once past 2.0x, only OVERTHINKING fires (OVER-ANALYSIS is suppressed). Each fires once at its threshold, not every subsequent thought. Implementation: track whether each has already fired via a simple comparison — if `thought_number` was below threshold on the previous thought (compare against `thought_number - 1`), this is the first crossing.

### generate_warnings() Entry Point

```rust
pub fn generate_warnings(
    input: &ThoughtInput,
    records: &[ThoughtRecord],
    config: &Config,
) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(check_language(&input.thought));
    warnings.extend(check_budget(input, records, config));
    warnings.extend(check_mode(input, config));
    warnings
}
```

### Phase 1 Change: Extract Lightweight Progress Data

`process_thought()` Phase 1 extracts a lightweight `RecentProgress` from the last 3 branch-filtered records. No full `ThoughtRecord` cloning needed — just two fields per record:

```rust
// In Phase 1, after branch filtering:
let recent_progress: RecentProgress = trace.thoughts
    .iter()
    .filter(|t| t.input.branch_id == input.branch_id)
    .rev()
    .take(3)
    .map(|t| (t.input.is_revision, t.input.branch_from_thought))
    .collect();
```

Add `recent_progress: RecentProgress` to `TraceSnapshot`. This avoids adding `Clone` to `ThoughtRecord`/`ThoughtResult` and keeps the Phase 1 extraction minimal.

### Dedup

After collecting all warnings, dedup by label within a single `generate_warnings()` call:

```rust
pub fn generate_warnings(...) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(check_language(&input.thought));
    warnings.extend(check_budget(input, &snapshot.recent_progress, config));
    warnings.extend(check_mode(input, config));

    // Dedup by label — keep first occurrence per [LABEL]
    let mut seen = std::collections::HashSet::new();
    warnings.retain(|w| {
        let label = w.split(']').next().unwrap_or("");
        seen.insert(label.to_owned())
    });
    warnings
}
```

## Test Strategy

All tests in `src/warnings.rs` `#[cfg(test)]`:

### Language Tests
- `test_anti_quick_fix_just_do`: thought = "let's just do a quick hack" → contains "ANTI-QUICK-FIX"
- `test_anti_quick_fix_should_be_fine`: thought = "should be fine" → contains "ANTI-QUICK-FIX"
- `test_dismissal_out_of_scope`: thought = "that's out of scope" → contains "DISMISSAL"
- `test_dismissal_known_issue`: thought = "it's a known issue" → contains "DISMISSAL"
- `test_clean_thought_no_warnings`: thought = "Let's analyze the trade-offs between PostgreSQL and Redis" → empty vec
- `test_case_insensitive`: thought = "JUST DO it" → still fires
- `test_dedup_same_label`: thought = "let's just do a quick hack" matches two ANTI-QUICK-FIX patterns → only one warning
- `test_false_positive_acknowledged`: thought = "we can simply use the existing trait implementation" → fires ANTI-QUICK-FIX (advisory, accepted)

### Budget Tests
- `test_over_analysis_fires`: thought 8 of 5, multiplier 1.5 → "OVER-ANALYSIS"
- `test_over_analysis_within_limit`: thought 7 of 5, multiplier 1.5 → no warning
- `test_overthinking_fires`: thought 11 of 5, multiplier 2.0, no branches in last 3 → "OVERTHINKING"
- `test_overthinking_suppressed_by_revision`: thought 11 of 5, but last record has is_revision=true → no "OVERTHINKING"
- `test_overthinking_suppressed_by_new_branch`: thought 11 of 5, but last record has branch_from_thought=Some(9) → no "OVERTHINKING"
- `test_underthinking_fires`: next_thought_needed=false, thought 1, mode="architecture" (budget min=5) → "UNDERTHINKING"
- `test_underthinking_ok_when_above_min`: next_thought_needed=false, thought 6, mode="architecture" → no warning
- `test_unknown_mode_fires_warning`: mode="nonexistent_mode" → "UNKNOWN-MODE"
- `test_no_mode_no_budget_warnings`: mode=None → no budget warnings at all
- `test_over_analysis_suppressed_when_overthinking`: thought 11 of 5 → only "OVERTHINKING", no "OVER-ANALYSIS"
- `test_over_analysis_fires_alone_at_threshold`: thought 8 of 5 (past 1.5x but under 2.0x) → "OVER-ANALYSIS" only

### Mode Tests
- `test_no_evidence_debugging`: mode="debugging", evidence=[] → "NO-EVIDENCE"
- `test_no_components_architecture`: mode="architecture", components=[] → "NO-COMPONENTS"
- `test_no_warning_when_fields_present`: mode="debugging", evidence=["file.rs"] → no warning
- `test_unknown_mode_no_mode_warnings`: mode="nonexistent" → no mode warnings (but UNKNOWN-MODE budget warning fires)
- `test_no_latency_custom_mode`: custom config with mode requiring "latency" → "NO-LATENCY" when missing
- `test_no_confidence_custom_mode`: custom config with mode requiring "confidence" → "NO-CONFIDENCE" when missing

### Integration Test
- `test_generate_warnings_merges_all`: thought with quick fix language + over budget + missing evidence → contains all 3 warning types

### Float Cast Test
- `test_budget_threshold_float_boundary`: thought 8 of 5, multiplier 1.5 → 8 > 7.5 fires. thought 7 of 5 → 7 < 7.5 does not.
