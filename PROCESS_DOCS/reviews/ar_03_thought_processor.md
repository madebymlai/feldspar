# Adversarial Review: Thought Processor — Recap, ADR, Eviction (03-thought-processor.md)

## Summary

Design is architecturally sound but has 4 critical issues: write lock held across async LLM call (blocks all traces), missing `response_format` (recaps silently fail), config rename breaks startup, and branch filtering for recap is unspecified. After amendments, ready for `/breakdown`.

**Reviewers**: ar-o (Opus), ar-k (Kimi/Valence), ar-glm5 (GLM-5.1)

## Critical (Must Address)

### Write lock held across async LLM call — blocks all concurrent traces
**Flagged by**: ar-o, ar-glm5 (2/3)  |  **Confidence**: High

`process_thought()` holds `traces.write().await` for its entire duration. Adding `generate_recap()` (async HTTP, up to 5 seconds) inside this lock blocks all other trace operations. Multiple concurrent agents would serialize behind a single recap call.

| Factor | Assessment |
|--------|------------|
| Severity | System failure at scale — all traces blocked |
| Probability | Guaranteed — every 3rd thought |
| Remediation Cost | Moderate — restructure into two phases |
| Reversibility | Must fix now — architectural |
| Context Fit | Multiple concurrent agents is core use case |

**Mitigation**: Two-phase approach: (1) Acquire write lock → append record → extract recap data (clone branch thought texts) → drop write lock. (2) Call LLM without lock. (3) Build wire response with recap result. Accept that trace may be extended concurrently — recap is best-effort.

### Missing `response_format: {"type": "json_object"}` in LLM API call
**Flagged by**: ar-o, ar-glm5 (2/3)  |  **Confidence**: High

Without `response_format` in the request body, `gpt-oss-20b:nitro` routes output to `reasoning_details` instead of `content`. The `chat_json()` method reads `content`, which will be `null`. Every recap silently fails.

| Factor | Assessment |
|--------|------------|
| Severity | System failure — all recaps return None |
| Probability | Guaranteed |
| Remediation Cost | Trivial — one line |
| Reversibility | Trivial |
| Context Fit | Core feature broken without this |

**Mitigation**: Add `"response_format": {"type": "json_object"}` to the request body in `chat_json()`.

### Config rename `[trace_review]` → `[llm]` breaks startup
**Flagged by**: ar-o, ar-k, ar-glm5 (3/3)  |  **Confidence**: High

`config/feldspar.toml` has `[trace_review]`. Renaming the struct field to `llm` without updating the TOML causes `Config::load()` to panic.

| Factor | Assessment |
|--------|------------|
| Severity | Server won't start |
| Probability | Guaranteed |
| Remediation Cost | Simple — update TOML + add serde alias |
| Reversibility | Trivial |
| Context Fit | CLAUDE.md says forward-first, but must be coordinated |

**Mitigation**: Update `config/feldspar.toml` in the same change. Add `#[serde(alias = "trace_review")]` on the `llm` field for transition safety. Update existing tests.

### Branch filtering for recap is unspecified
**Flagged by**: ar-k, ar-glm5 (2/3)  |  **Confidence**: High

Brief says "recap scoped to current branch" but no algorithm is specified. When `branch_id` is None (main line), unclear whether to include all thoughts or only main-line thoughts. When on a branch, unclear whether to include pre-branch main-line thoughts.

| Factor | Assessment |
|--------|------------|
| Severity | Wrong recap content |
| Probability | Common — branching is a core feature |
| Remediation Cost | Simple — specify the algorithm |
| Reversibility | Fixable later but wrong content is worse than no content |
| Context Fit | Recap quality is the whole point of this feature |

**Mitigation**: Specify: if `branch_id == None`, include thoughts where `branch_id.is_none()`. If `branch_id == Some(b)`, include thoughts where `branch_id == Some(b)`.

## Recommended (High Value)

### LlmClient::new() should not panic on build failure
**Flagged by**: ar-o, ar-glm5  |  **Confidence**: Medium

`reqwest::Client::builder().build().expect()` panics the entire server on TLS issues. Return `Option<LlmClient>` instead, with a warning log.

### ADR alternatives should use branch first-thought text, not branch IDs
**Flagged by**: ar-o, ar-glm5  |  **Confidence**: Medium

"alt-1" in an ADR is meaningless. Extract the first thought text from each branch as the alternative description.

### chat_json should log failures, not silently swallow them
**Flagged by**: ar-o, ar-k  |  **Confidence**: Medium

Use `tracing::warn!` on HTTP errors, parse failures, and timeouts. "Best-effort" doesn't mean "invisible."

## Noted (Awareness)

- `base_url` trailing slash produces double-slash in URL — trim before storing
- Missing `connect_timeout` — add `connect_timeout(Duration::from_secs(5))` alongside the total timeout
- `recap_every = 1` has no guard — consider minimum of 2, or document the risk
- ADR `HashSet::into_iter()` is non-deterministic — use `BTreeSet` or sort after collecting
- Eviction `traces.remove().unwrap()` — use `if let` for safety margin
- `trace_review.rs` module fate — document that it will import `LlmClient` in issue #5
- No test for concurrent thoughts during recap (regression test for lock fix)
- `api_key_env` Optional may affect trace_review.rs stub — document

## Recommendation

[x] REVISE — 4 Critical issues require design amendments before `/breakdown`
[ ] PROCEED
