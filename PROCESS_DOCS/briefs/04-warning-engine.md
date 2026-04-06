# Brief: Warning Engine

**Problem**: Claude cuts corners — uses shortcut language, wraps up too early on complex decisions, skips evidence in debugging mode. The warning engine catches these patterns and pushes back via advisory warnings in the tool response. Claude sees the warnings and self-corrects. This is the "tech lead looking over its shoulder" from the Reddit post.

**Requirements**:

**Language warnings** (`src/warnings.rs`):
- ANTI-QUICK-FIX: regex for shortcut language
  - `\b(just|simply)\s+(do|use|add|skip|ignore|throw|hack|slap)`
  - `\bquick\s*(fix|solution|hack)`
  - `\bgood\s+enough`, `\bshould\s+be\s+fine`
- Dismissal language:
  - `\bpre.?existing\s+(issue|problem|bug)`
  - `\bout\s+of\s+scope`, `\bnot\s+(my|our)\s+(problem|concern)`
  - `\b(already|was)\s+broken`, `\bworked\s+before`, `\bknown\s+issue`

**Budget warnings**:
- OVER-ANALYSIS: `thoughtNumber > totalThoughts * config.thresholds.over_analysis_multiplier` (1.5)
- OVERTHINKING: `thoughtNumber > totalThoughts * config.thresholds.overthinking_multiplier` (2.0) with no new branches/revisions in last 3 thoughts
- UNDERTHINKING: `nextThoughtNeeded=false` before reaching budget minimum for the mode tier

**Mode-specific warnings**:
- Read mode config from `config/feldspar.toml` `[modes.*]` sections
- Check `requires` fields and fire warnings when required fields are missing:
  - `"evidence"` required → NO-EVIDENCE if `evidence` array is empty
  - `"components"` required → NO-COMPONENTS if `affected_components` is empty
  - Extend for any future `requires` values
- `watches` field is documentation only — not enforced by the warning engine

**Merging**:
- Warning engine produces `Vec<String>`
- Analyzer alerts (from issue #5) get converted to warning strings and appended to the same array
- Both merged into the `warnings` field in `WireResponse`

**Entry point**: `generate_warnings(input: &ThoughtInput, records: &[ThoughtRecord], config: &Config) -> Vec<String>`

**Constraints**:
- Pure sync, no LLM calls, no async — regex matching and config lookups only
- All thresholds from `config/feldspar.toml` — no hardcoded magic numbers
- All warnings advisory — Claude can override with justification
- No cap, no dedup, no escalation — all warnings fire every time they match
- Regex compiled once (lazy_static or OnceLock), not per-thought

**Non-goals**:
- Analyzer pipeline (issue #5)
- Enforcing `watches` field (future work, may need LLM)
- Blocking or gating — warnings are never hard stops
- Trace review or ML (issues #7, #8)

**Style**: Terse warning strings that Claude reads mid-reasoning. Format: `"WARNING [LABEL]: message"`. Short, actionable, no fluff.

**Key concepts**:
- **Warning**: Advisory string injected into tool response. Claude sees it and adjusts.
- **Budget tier**: minimal/standard/deep — derived from mode config, determines thought budget range.
- **Mode validation**: Each thinking mode declares required fields. Missing fields trigger warnings.
- **Requires**: Machine-readable list of required input fields per mode (`["evidence"]`, `["components"]`).
- **Watches**: Human-readable description of what good reasoning looks like. Not enforced.
