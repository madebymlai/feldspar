# Solution Design: Thought Processor — Recap, ADR, Eviction

## 1. Executive Summary

Wire real recap generation (LLM via OpenRouter), ADR skeleton (template), and trace eviction (remove + Arc + spawn) into the existing `process_thought()` pipeline. Add a provider-agnostic `LlmClient` for OpenAI-compatible APIs. Rename `[trace_review]` config to `[llm]` with `base_url` support. Single new module `src/llm.rs`.

## 2. Rationale

| Decision | Rationale | Alternative | Why Rejected |
|----------|-----------|-------------|--------------|
| LLM recap via `gpt-oss-20b:nitro` | Tested: 300ms, $0.00005, good quality. JSON format forces content field. | Template concatenation | User chose LLM after testing |
| Template ADR (not LLM) | User decided — instant, free, skeleton is just formatted trace data | LLM-generated ADR | Cost + latency for a skeleton |
| Provider-agnostic `LlmClient` | OpenAI-compatible API is de facto standard. Works with OpenRouter, OpenAI, Ollama, vLLM, Together, Groq. | OpenRouter-specific client | Locks users into one provider |
| `[llm]` config section (rename from `[trace_review]`) | Client is shared by recap + trace review + future features. Name should reflect actual scope. | Keep `[trace_review]` | Misleading — it's not just trace review anymore |
| `base_url` defaults to OpenRouter | Most users will use OpenRouter. Local model users change the URL. | Hardcode OpenRouter | Breaks local/self-hosted models |
| Shared `reqwest::Client` on ThinkingServer | Connection pooling, reused by recap + trace review. 5-second total + 5-second connect timeout. | Per-request client | Wastes connections |
| Two-phase process_thought | Drop write lock before LLM call. Prevents blocking all traces during recap. | Hold lock entire method | Blocks all concurrent traces for up to 5 seconds |
| `response_format: json_object` in API call | Forces reasoning models to put output in `content` field, not `reasoning_details`. Tested requirement. | Prompt-only JSON trick | Model returns `content: null` without this |
| `#[serde(alias = "trace_review")]` on llm field | Forward-first rename, but accepts old config during transition | Hard rename only | Existing configs break silently |
| `BTreeSet` instead of `HashSet` for ADR fields | Deterministic ordering in ADR output | `HashSet` | Non-deterministic — same trace produces different ADRs |
| `LlmClient::new()` returns `Option<LlmClient>` | TLS build failure shouldn't crash the server | `expect()` | Panics on misconfigured systems |
| `recap_every` minimum of 2 | `recap_every = 1` means LLM call on every thought — expensive, slow | No guard | Silent performance degradation |
| Eviction via `HashMap::remove()` + `Arc` | Idiomatic tokio pattern. No clone of full trace. Background tasks get `Arc<Trace>` ownership. | Clone then evict | Unnecessary full data copy |
| Recap failure = skip | Best-effort per CLAUDE.md. Recap is useful but not critical. | Retry with backoff | Adds latency for marginal benefit |
| 5-second timeout on LLM calls | Typical response is 300ms. 5s is generous cap. | No timeout | Could hang on network issues |

## 3. Technology Stack

| Component | Crate | Version | Purpose |
|-----------|-------|---------|---------|
| HTTP client | `reqwest` | 0.12 (json) | Already in Cargo.toml. LLM API calls. |
| Async runtime | `tokio` | 1 (full) | Already in Cargo.toml. spawn for background tasks. |
| JSON | `serde_json` | 1 | Already in Cargo.toml. Parse LLM responses. |

**No new dependencies.** Everything needed is already in Cargo.toml.

**Config changes:**
- `config/feldspar.toml`: Rename `[trace_review]` → `[llm]`, add `base_url`
- `src/config.rs`: Rename `TraceReviewConfig` → `LlmConfig`, add `base_url` field

## 4. Architecture

### Data Flow

```
process_thought() receives ThoughtInput — TWO-PHASE DESIGN (never hold write lock across LLM call)

PHASE 1 (write lock held — microseconds only):
  → acquire traces.write().await
  → create/lookup trace, append record
  → compute budget from config
  → extract recap data if needed: clone branch-filtered thought texts as String
  → extract ADR data if completing: clone trace data needed for template
  → if !next_thought_needed: traces.remove(&trace_id) → owned Trace
  → DROP write lock (explicit drop(traces))

PHASE 2 (no lock — async LLM call safe here):
  → if recap due (thought_number % recap_every == 0):
      llm_client.chat_json(recap_prompt, recap_text) → {"recap": "..."}
      on failure: tracing::warn!, skip
  → if completing:
      generate_adr() from extracted data → String
      Arc::new(removed_trace)
      tokio::spawn no-op background tasks with Arc clones
  → build WireResponse from extracted data + recap + adr
  → return WireResponse
```

**Branch filtering for recap**: 
- If `input.branch_id.is_none()` (main line): include only thoughts where `branch_id.is_none()`
- If `input.branch_id == Some(b)`: include only thoughts where `branch_id == Some(b)`
- Filter applied before formatting as numbered text for the LLM prompt

### Module Catalog

**`src/llm.rs`** (new) — Provider-agnostic LLM client

| Component | Role |
|-----------|------|
| `LlmClient` | Shared HTTP client for any OpenAI-compatible API |
| `LlmClient::new()` | Build from LlmConfig, create reqwest::Client with 5s timeout |
| `LlmClient::chat_json()` | POST /chat/completions, parse JSON from content field |

**`src/thought.rs`** — Modifications to existing code

| Change | Role |
|--------|------|
| Add `llm: Option<LlmClient>` to ThinkingServer | Shared LLM client |
| `generate_recap()` | Async — call LlmClient, parse `{"recap": "..."}` |
| `generate_adr()` | Sync — template string from trace data |
| Eviction in `process_thought()` | Remove from HashMap, Arc-wrap, spawn background tasks |

**`src/config.rs`** — Rename + extend

| Change | Role |
|--------|------|
| `TraceReviewConfig` → `LlmConfig` | Rename struct |
| Add `base_url: Option<String>` | Defaults to `https://openrouter.ai/api/v1` if absent |
| `Config.trace_review` → `Config.llm` | Rename field with `#[serde(alias = "trace_review")]` for transition |
| Validate `recap_every >= 2` | Prevent LLM call on every thought |

**`config/feldspar.toml`** — Rename section

```toml
[llm]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
model = "openai/gpt-oss-20b:nitro"
```

## 5. Protocol/Schema

### LlmClient

```rust
pub struct LlmClient {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl LlmClient {
    /// Returns None on build failure (TLS issues, etc) — logs warning, server continues without LLM.
    pub fn new(config: &LlmConfig) -> Option<Self> {
        let api_key = config.api_key_env.as_deref()
            .and_then(|env_name| std::env::var(env_name).ok());

        let base_url = config.base_url.clone()
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".into())
            .trim_end_matches('/')  // prevent double-slash in URL
            .to_owned();

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("failed to build LLM HTTP client: {}", e);
                return None;
            }
        };

        Some(Self { client, base_url, api_key, model: config.model.clone() })
    }

    /// Send a chat completion request with JSON response format.
    /// Returns parsed JSON content or None on failure. Logs all failures.
    pub async fn chat_json(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Option<serde_json::Value> {
        let url = format!("{}/chat/completions", self.base_url);

        let mut req = self.client.post(&url)
            .header("Content-Type", "application/json");

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "max_tokens": max_tokens,
            "response_format": {"type": "json_object"}
        });

        let resp = match req.json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("LLM request failed: {}", e);
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!("LLM returned HTTP {}", resp.status());
            return None;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("LLM response parse failed: {}", e);
                return None;
            }
        };

        let content = match json["choices"][0]["message"]["content"].as_str() {
            Some(c) => c,
            None => {
                tracing::warn!("LLM response missing content field");
                return None;
            }
        };

        match serde_json::from_str(content) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("LLM content JSON parse failed: {}", e);
                None
            }
        }
    }
}
```

### LlmConfig (renamed from TraceReviewConfig)

```rust
#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,  // now Optional — local models don't need keys
    pub model: String,
}
```

### Recap prompt

```
System: "You summarize thinking traces. Given numbered thoughts, produce a 1-2 sentence
recap capturing the key progression and current conclusion. Respond with ONLY a JSON
object: {"recap": "<your summary>"}"

User: "Thought 1: {text}\n\nThought 2: {text}\n\n..."
```

### ADR template

```rust
fn generate_adr(trace: &Trace) -> String {
    let date = /* current date YYYY-MM-DD */;

    // BTreeSet for deterministic ordering
    let components: Vec<String> = trace.thoughts.iter()
        .flat_map(|t| t.input.affected_components.iter().cloned())
        .collect::<BTreeSet<_>>().into_iter().collect();
    let modes: Vec<String> = trace.thoughts.iter()
        .filter_map(|t| t.input.thinking_mode.clone())
        .collect::<BTreeSet<_>>().into_iter().collect();

    // Decision = last main-line thought (branch_id.is_none()), not last chronological thought
    let decision = trace.thoughts.iter()
        .filter(|t| t.input.branch_id.is_none())
        .last()
        .map(|t| t.input.thought.as_str())
        .unwrap_or("No conclusion");

    // Branches explored = first thought text from each branch (not just branch IDs)
    let mut branch_descriptions: Vec<String> = Vec::new();
    let mut seen_branches = BTreeSet::new();
    for t in &trace.thoughts {
        if let Some(ref bid) = t.input.branch_id {
            if seen_branches.insert(bid.clone()) {
                // First thought on this branch — use as description
                let desc = if t.input.thought.len() > 100 {
                    format!("{}: {}...", bid, &t.input.thought[..100])
                } else {
                    format!("{}: {}", bid, t.input.thought)
                };
                branch_descriptions.push(desc);
            }
        }
    }

    format!(
        "## ADR\n**Date**: {}\n**Components**: {}\n**Mode**: {}\n**Decision**: {}\n**Branches explored**: {}",
        date,
        if components.is_empty() { "none".into() } else { components.join(", ") },
        if modes.is_empty() { "none".into() } else { modes.join(", ") },
        decision,
        if branch_descriptions.is_empty() { "none".into() } else { branch_descriptions.join("; ") },
    )
}
```

### Eviction pattern

```rust
// In process_thought() PHASE 1 — while write lock is held:
if !input.next_thought_needed {
    if let Some(trace) = traces.remove(&trace_id) {
        // Extract ADR data before dropping lock
        let adr_data = /* clone what generate_adr needs */;
        // Store removed trace for Phase 2
        removed_trace = Some(trace);
    }
}
drop(traces); // EXPLICIT DROP — release write lock before any async work

// PHASE 2 — no lock held:
if let Some(trace) = removed_trace {
    let adr = generate_adr(&adr_data);
    let trace = Arc::new(trace);

    // No-op background tasks — real implementations wired by issues #5-#7
    let t1 = trace.clone();
    tokio::spawn(async move { let _ = &t1; /* db_flush */ });
    let t2 = trace.clone();
    tokio::spawn(async move { let _ = &t2; /* trace_review */ });
    tokio::spawn(async move { let _ = &trace; /* ml_train */ });
}
```

## 6. Implementation Details

### File Structure

```
src/llm.rs       ← NEW: LlmClient, chat_json()
src/thought.rs   ← MODIFY: add llm field, generate_recap(), generate_adr(), eviction
src/config.rs    ← MODIFY: TraceReviewConfig → LlmConfig, add base_url
src/main.rs      ← MODIFY: create LlmClient, pass to ThinkingServer
config/feldspar.toml ← MODIFY: [trace_review] → [llm], add base_url
```

### Integration Points

- `ThinkingServer` gets `llm: Option<LlmClient>` field (Option because tests can run without API key)
- `main.rs` creates `LlmClient::new(&config.llm)` and passes it to ThinkingServer
- `mcp.rs` unchanged — it calls `process_thought()` which handles everything internally
- `generate_recap()` calls `self.llm.as_ref()?.chat_json(...)` — None if no LLM client
- Config validation: `[llm].model` is required, `base_url` and `api_key_env` are optional

### Module notes

- **`src/trace_review.rs`** — existing stub declares `mod trace_review;` in main.rs. This issue does NOT modify it. Issue #5 (Trace Review) will update it to import `LlmClient` from `src/llm.rs` and implement the trust scoring logic. The module name stays `trace_review` even though the config section is now `[llm]` — the module describes the *feature*, the config describes the *infrastructure*.

### Issues that update this code later

| Issue | What changes |
|-------|-------------|
| #5 (Trace Review) | `trace_review.rs` imports `LlmClient`, implements trust scoring in background spawn |
| #6 (DB) | Replace no-op `db_flush` spawn with real SQLite write |
| #7 (ML) | Replace no-op `ml_train` spawn with real PerpetualBooster training |

### Test Plan

```rust
// src/llm.rs #[cfg(test)]
- test_llm_client_constructs: LlmClient::new() with test config → Some(client)
- test_llm_client_none_on_build_failure: (if testable) verify graceful None return
- test_chat_json_url_format: verify base_url + "/chat/completions" concatenation
- test_base_url_trailing_slash_trimmed: "https://example.com/v1/" → no double slash

// src/thought.rs #[cfg(test)]
- test_generate_adr_basic: trace with 2 main-line thoughts, verify ADR contains date, components, decision
- test_generate_adr_decision_from_mainline: trace with main + branch thoughts, verify decision is last main-line thought (not last branch)
- test_generate_adr_with_branches: trace with branches, verify "Branches explored" has first thought text from each branch
- test_generate_adr_no_components: verify "none" when no components
- test_generate_adr_deterministic: same trace twice → same ADR output (BTreeSet ordering)
- test_recap_skipped_without_llm: process_thought with llm=None, verify recap is None
- test_recap_on_third_thought: mock/verify recap called on thought 3 (recap_every=3)
- test_recap_branch_filtering: thoughts on branch "alt-1" + main line, recap for main line excludes branch thoughts
- test_eviction_removes_trace: process thought 1, close with thought 2, verify trace removed from HashMap
- test_eviction_map_empty_after_close: after closing, traces HashMap is empty
- test_process_thought_adr_on_completion: close trace, verify wire.adr is Some
- test_concurrent_thoughts_during_recap: two traces, one doing recap, other not blocked (verifies lock is released)

// src/config.rs #[cfg(test)]
- test_llm_config_parses: verify [llm] section parsed correctly
- test_llm_config_alias_trace_review: verify [trace_review] still parses via serde alias
- test_llm_config_optional_base_url: omit base_url, verify None
- test_llm_config_optional_api_key_env: omit api_key_env, verify None
- test_recap_every_minimum: recap_every = 1 → validation panics
- update existing tests: rename trace_review references to llm
```

### Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| OpenRouter timeout on recap | Low | Low | 5s total + 5s connect timeout, skip on failure, tracing::warn | Recap is None + log warning |
| API key missing | Medium | Low | `api_key_env` Optional, auth header skipped if None | Works for local models |
| Config rename breaks existing configs | Guaranteed | Low | `#[serde(alias = "trace_review")]` accepts both names | test_llm_config_alias_trace_review |
| Write lock blocking concurrent traces | N/A (fixed) | N/A | Two-phase design: drop lock before LLM call | test_concurrent_thoughts_during_recap |
| LLM returns content: null | N/A (fixed) | N/A | `response_format: json_object` forces content field | Tested with gpt-oss-20b:nitro |
| TLS build failure crashes server | Low | High | LlmClient::new() returns Option, warns on failure | Server starts without LLM |
| Non-deterministic ADR output | N/A (fixed) | N/A | BTreeSet for sorted, deterministic ordering | test_generate_adr_deterministic |
