# Build Agent 1: Config Migration + LLM Client

## Dependencies

None (parallel) — foundational changes that Agent 2 depends on.

## Overview

- **Objective**: Rename `[trace_review]` config to `[llm]` with transition alias, add `base_url` field, implement provider-agnostic `LlmClient` in new `src/llm.rs`, update validation.
- **Scope**:
  - Includes: `src/config.rs`, `src/llm.rs` (new), `config/feldspar.toml`
  - Excludes: No changes to `src/thought.rs`, `src/mcp.rs`, `src/main.rs` (Agent 2 handles those)
- **Dependencies**:
  - Existing: `src/config.rs` (from issue #1), `config/feldspar.toml`
  - Crates: `reqwest` 0.12 (json), `serde`/`serde_json` 1, `tracing` 0.1 — all in Cargo.toml
- **Estimated Complexity**: Low — struct rename, one new module with HTTP client

## Technical Approach

### Config Migration
- Rename `TraceReviewConfig` → `LlmConfig`
- Rename `Config.trace_review` → `Config.llm` with `#[serde(alias = "trace_review")]`
- Add `base_url: Option<String>` to LlmConfig
- Make `api_key_env` Optional (local models don't need keys)
- Update `config/feldspar.toml`: `[trace_review]` → `[llm]` with `base_url`
- Add validation: `recap_every >= 2`

### LLM Client
- Provider-agnostic: works with any OpenAI-compatible API (OpenRouter, OpenAI, Ollama, vLLM)
- `LlmClient::new()` returns `Option<Self>` (no panic on TLS failure)
- `chat_json()` sends `response_format: {"type": "json_object"}` to force content field
- Logs all failures with `tracing::warn!`
- `connect_timeout` + total timeout of 5 seconds each
- Trims trailing slash from `base_url`

---

## Task Breakdown

### Task 1: Rename TraceReviewConfig → LlmConfig in config.rs

- **Description**: Rename the struct and field, add alias for backward compatibility, add base_url, make api_key_env Optional.
- **Acceptance Criteria**:
  - [ ] `TraceReviewConfig` renamed to `LlmConfig`
  - [ ] `Config.trace_review` renamed to `Config.llm`
  - [ ] `#[serde(alias = "trace_review")]` on the `llm` field
  - [ ] `base_url: Option<String>` field added to LlmConfig
  - [ ] `api_key_env` changed from `String` to `Option<String>`
  - [ ] `cargo check` passes
- **Files to Modify**:
  ```
  src/config.rs
  ```
- **Dependencies**: None
- **Code — LlmConfig struct**:
  ```rust
  #[derive(Debug, Deserialize)]
  pub struct LlmConfig {
      pub base_url: Option<String>,
      pub api_key_env: Option<String>,
      pub model: String,
  }
  ```
- **Code — Config field with alias**:
  ```rust
  #[derive(Debug, Deserialize)]
  pub struct Config {
      pub feldspar: FeldsparConfig,
      #[serde(alias = "trace_review")]
      pub llm: LlmConfig,
      // ... rest unchanged
  }
  ```
- **Test Cases** (file: `src/config.rs` `#[cfg(test)]`):
  - **`test_llm_config_parses`**: Load `config/feldspar.toml` → `config.llm.model == "openai/gpt-oss-20b:nitro"`
  - **`test_llm_config_alias_trace_review`**: Create TOML string with `[trace_review]` section → parses into `config.llm` successfully
  - **`test_llm_config_optional_base_url`**: TOML without `base_url` → `config.llm.base_url.is_none()`
  - **`test_llm_config_optional_api_key_env`**: TOML without `api_key_env` → `config.llm.api_key_env.is_none()`
  - **Update existing tests**: Replace all `trace_review` references with `llm` in `test_config()` helper and assertions. The `test_config()` helper currently has a `TraceReviewConfig` — rename to `LlmConfig`.

---

### Task 2: Update config/feldspar.toml

- **Description**: Rename the `[trace_review]` section to `[llm]` and add `base_url`.
- **Acceptance Criteria**:
  - [ ] `[trace_review]` renamed to `[llm]`
  - [ ] `base_url` field added
  - [ ] Existing fields preserved
- **Files to Modify**:
  ```
  config/feldspar.toml
  ```
- **Dependencies**: Task 1
- **Code**:
  ```toml
  [llm]
  base_url = "https://openrouter.ai/api/v1"
  api_key_env = "OPENROUTER_API_KEY"
  model = "openai/gpt-oss-20b:nitro"
  ```

---

### Task 3: Add recap_every validation (minimum 2)

- **Description**: Update the `validate()` function to enforce `recap_every >= 2`.
- **Acceptance Criteria**:
  - [ ] `validate()` panics if `recap_every < 2`
  - [ ] Panic message: `"recap_every must be >= 2 (LLM call per thought is too expensive)"`
  - [ ] Existing `recap_every > 0` check replaced
- **Files to Modify**:
  ```
  src/config.rs
  ```
- **Dependencies**: Task 1
- **Code**: Replace `assert!(config.feldspar.recap_every > 0, "recap_every must be > 0");` with:
  ```rust
  assert!(config.feldspar.recap_every >= 2, "recap_every must be >= 2 (LLM call per thought is too expensive)");
  ```
- **Test Cases**:
  - **`test_recap_every_one_panics`**: Set `recap_every = 1` → `#[should_panic(expected = "recap_every must be >= 2")]`
  - **Update**: `test_recap_every_zero_panics` still works (0 < 2)
  - **Note**: The existing `config/feldspar.toml` has `recap_every = 3` which is valid.

---

### Task 4: Implement LlmClient in src/llm.rs

- **Description**: Create new module with provider-agnostic LLM client.
- **Acceptance Criteria**:
  - [ ] `LlmClient::new()` returns `Option<LlmClient>` (None on build failure, logs warning)
  - [ ] `chat_json()` sends `response_format: {"type": "json_object"}`
  - [ ] `chat_json()` logs all failure modes with `tracing::warn!`
  - [ ] `connect_timeout(5s)` + `timeout(5s)` on client builder
  - [ ] `base_url` trailing slash trimmed
  - [ ] Auth header skipped when `api_key` is None
  - [ ] `cargo check` passes
- **Files to Create**:
  ```
  src/llm.rs    ← NEW
  ```
- **Dependencies**: Task 1 (imports LlmConfig)
- **Code**:
  ```rust
  use crate::config::LlmConfig;
  use serde_json::Value;
  use std::time::Duration;

  pub struct LlmClient {
      client: reqwest::Client,
      base_url: String,
      api_key: Option<String>,
      model: String,
  }

  impl LlmClient {
      pub fn new(config: &LlmConfig) -> Option<Self> {
          let api_key = config.api_key_env.as_deref()
              .and_then(|env_name| std::env::var(env_name).ok());

          let base_url = config.base_url.clone()
              .unwrap_or_else(|| "https://openrouter.ai/api/v1".into())
              .trim_end_matches('/')
              .to_owned();

          let client = match reqwest::Client::builder()
              .timeout(Duration::from_secs(5))
              .connect_timeout(Duration::from_secs(5))
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

      pub async fn chat_json(
          &self,
          system: &str,
          user: &str,
          max_tokens: u32,
      ) -> Option<Value> {
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

          let json: Value = match resp.json().await {
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
- **Important**: Add `mod llm;` to `src/main.rs`. Since Agent 2 owns `main.rs`, coordinate: Agent 1 creates `src/llm.rs`, Agent 2 adds `mod llm;` to `main.rs`. Alternatively, add `pub mod llm;` here and Agent 2 uses `use crate::llm::LlmClient;`.

  **Resolution**: Agent 1 should NOT modify `main.rs`. Instead, create `src/llm.rs` as a standalone file. Agent 2 will add `mod llm;` to `main.rs` when it wires everything up.
- **Test Cases** (file: `src/llm.rs` `#[cfg(test)]`):
  - **`test_llm_client_constructs`**: Create LlmConfig with `model: "test"`, `base_url: Some("http://localhost:11434/v1")`, `api_key_env: None`. Call `LlmClient::new()`. Assert `Some(client)`.
  - **`test_base_url_trailing_slash_trimmed`**: Create LlmConfig with `base_url: Some("http://localhost/v1/".into())`. Call `LlmClient::new()`. The client should store `"http://localhost/v1"` (no trailing slash). Verify via a helper or by checking URL construction.
  - **`test_base_url_defaults_to_openrouter`**: Create LlmConfig with `base_url: None`. Call `LlmClient::new()`. Verify internal base_url is `"https://openrouter.ai/api/v1"`.

  **Note**: Testing `chat_json` with real HTTP calls requires a running server. Skip live API tests in unit tests. The integration test in Agent 2 can test the full flow if `OPENROUTER_API_KEY` is set.

---

### Task 5: Verify all tests pass

- **Description**: Run `cargo test` and verify all existing + new tests pass.
- **Acceptance Criteria**:
  - [ ] All existing 53 tests pass (config, thought, mcp)
  - [ ] New config tests pass (llm rename, alias, optional fields, recap_every)
  - [ ] New llm tests pass (construction, base_url)
  - [ ] `cargo check` passes with no errors
  - [ ] Only expected dead_code warnings
- **Dependencies**: Tasks 1-4
- **Note**: `src/llm.rs` won't be linked into the binary yet (no `mod llm;` in main.rs). To run its tests, temporarily add `mod llm;` or use `#[cfg(test)]` path. Actually — since Agent 2 will add `mod llm;`, the tests in `src/llm.rs` won't run until Agent 2 completes. Agent 1 should verify `cargo check` passes and config tests pass. LLM tests will be verified by Agent 2.

  **Alternative**: Add `mod llm;` to `main.rs` in this agent. This is a one-line change that doesn't conflict with Agent 2's work (Agent 2 modifies other parts of main.rs). This ensures LLM tests run in Agent 1.

---

## Testing Strategy

- **Framework**: Rust `#[cfg(test)]`, `cargo test`
- **Structure**: Tests inline in `src/config.rs` and `src/llm.rs`
- **Coverage**: ~8 new config tests + ~3 new llm tests + all 53 existing tests
- **Run**: `cargo test`

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Detection |
|------|------------|--------|------------|-----------|
| serde alias doesn't work with toml crate | Low | High | Test explicitly with both section names | test_llm_config_alias_trace_review |
| Existing mcp.rs tests break from config rename | Medium | Low | mcp.rs tests use `test_config()` helper — update it | cargo test |
| reqwest client build fails in CI | Low | Medium | `new()` returns Option, warns | test_llm_client_constructs |

## Success Criteria

- [ ] `Config.llm` field parses from both `[llm]` and `[trace_review]` TOML sections
- [ ] `LlmClient::new()` returns `Some` with valid config, handles build failure gracefully
- [ ] `chat_json` includes `response_format: json_object` in request body
- [ ] `recap_every < 2` panics at startup
- [ ] All tests pass

## Implementation Notes

- **Do NOT modify `src/thought.rs`, `src/mcp.rs`** — Agent 2 handles those
- **`src/main.rs`**: Only add `mod llm;` — nothing else. Agent 2 handles the rest.
- **Existing `test_config()` helper** in config.rs tests builds a Config programmatically. It currently has `trace_review: TraceReviewConfig { ... }`. Rename to `llm: LlmConfig { ... }`.
- **Existing mcp.rs `test_config()` helper** also builds a Config. It will break from the rename. Since we can't modify mcp.rs, ensure the struct field rename compiles — mcp.rs tests should use `config.llm` not `config.trace_review`. BUT — we're not touching mcp.rs. If mcp.rs has its own `test_config()` that references `trace_review`, it will fail. Check if it does and handle accordingly.
- **`pub use`**: Make `LlmClient` and `LlmConfig` pub so other modules can import them.
