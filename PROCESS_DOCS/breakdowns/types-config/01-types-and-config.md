# Build Agent 1: Types + Config

## Dependencies

None (parallel) — this is the foundation layer, no prior agents required.

## Overview

- **Objective**: Implement all core Rust type definitions and the TOML/YAML config loader for feldspar. Every other module in the project imports from these two files.
- **Scope**:
  - Includes: `src/thought.rs` (all types), `src/config.rs` (config loader + validator + principles loader), `Cargo.toml` (add serde-saphyr), inline tests for both modules
  - Excludes: No thought processing logic, no analyzer logic, no DB/ML logic, no MCP server wiring, no methods on ThinkingServer beyond `new()`
- **Dependencies**:
  - Crates already in Cargo.toml: `serde` (1, derive), `serde_json` (1), `tokio` (1, full), `uuid` (1, v4), `toml` (0.8)
  - Crate to add: `serde-saphyr` (latest stable)
  - Dev dependency already present: `assert_matches` (1.5)
  - Config files that must exist (already do): `config/feldspar.toml`, `config/principles.yaml`
- **Estimated Complexity**: Low — all type definitions with derives, one config loader with validation, no complex behavior

## Technical Approach

### Architecture Decisions

| Decision | Rationale |
|----------|-----------|
| Input/output struct separation | ThoughtInput = Deserialize only (from Claude), ThoughtResult = Serialize + Deserialize (to wire + DB round-trip) |
| `rename_all = "camelCase"` on wire types | MCP JSON uses camelCase; Rust uses snake_case internally |
| `type Timestamp = i64` | Unix millis, maps to SQLite INTEGER, no chrono dependency |
| Two-stage config parse | Stage 1: serde into raw structs. Stage 2: semantic cross-reference validation |
| Two-stage principles YAML | Stage 1: parse into `RawPrinciples` (HashMap). Stage 2: map keys to `PrincipleGroup.name` |
| `Option<DbPool>` / `Option<MlModel>` | Empty marker types, None until issues #5/#6 wire real implementations |
| `RwLock<HashMap<String, Trace>>` | `tokio::sync::RwLock` — reads >> writes, no extra dependency |
| `Arc<Config>` | Loaded once at startup, immutable, cloned to async handlers |

### Module Placement

```
src/config.rs  ← Config, FeldsparConfig, TraceReviewConfig, ThresholdsConfig,
                  PruningConfig, ModeConfig, ComponentsConfig, RawPrinciples,
                  RawPrincipleGroup, PrincipleGroup, Principle,
                  Config::load(), validate(), load_principles()

src/thought.rs ← Timestamp, ThoughtInput, ThoughtResult, ThoughtRecord,
                  Impact, Alert, Severity, Trace, DbPool, MlModel,
                  ThinkingServer, ThinkingServer::new()

Cargo.toml     ← add serde-saphyr under [dependencies]
```

### Data Flow

```
Startup:
  Config::load("config/feldspar.toml", "config/principles.yaml")
    → read TOML → toml::from_str → Config struct (minus principles)
    → read YAML → serde_saphyr::from_str → RawPrinciples
    → map HashMap keys to PrincipleGroup.name, filter active only
    → validate cross-references (budgets, modes, requires, principles)
    → panic on any validation failure
    → Arc::new(config) → immutable forever

  ThinkingServer::new(config)
    → empty RwLock<HashMap>, db: None, ml: None
```

---

## Task Breakdown

### Task 1: Add serde-saphyr dependency to Cargo.toml

- **Description**: Add the `serde-saphyr` crate for YAML parsing (replaces deprecated `serde_yaml`).
- **Acceptance Criteria**:
  - [ ] `serde-saphyr` added under `[dependencies]` in `Cargo.toml`
  - [ ] `cargo check` passes with no errors
- **Files to Modify**:
  ```
  Cargo.toml
  ```
- **Dependencies**: None
- **Code Example**:
  ```toml
  # Add after the toml line in [dependencies]:
  serde-saphyr = "1"
  ```

---

### Task 2: Implement config types and loader in src/config.rs

- **Description**: Replace the comment stub in `src/config.rs` with all config structs, the `Config::load()` entry point, `validate()` function, and `load_principles()` function.
- **Acceptance Criteria**:
  - [ ] All config structs defined with correct serde derives
  - [ ] `Config::load(toml_path, principles_path)` returns `Arc<Config>`
  - [ ] `load_principles()` does two-stage YAML deserialization (RawPrinciples → PrincipleGroup)
  - [ ] YAML map keys become `PrincipleGroup.name` values
  - [ ] Only active principle groups included in final Config
  - [ ] `validate()` checks all 7 semantic rules and panics with descriptive messages
  - [ ] `cargo check` passes
- **Files to Create/Modify**:
  ```
  src/config.rs  ← replace stub entirely
  ```
- **Dependencies**: Task 1 (serde-saphyr in Cargo.toml)
- **Configuration Required**:
  ```rust
  /// Valid values for ModeConfig.requires — closed set
  const VALID_REQUIRES: &[&str] = &["components", "evidence", "latency", "confidence"];
  ```
- **Code — Config struct and sub-structs**:
  ```rust
  use serde::Deserialize;
  use std::collections::HashMap;
  use std::sync::Arc;

  #[derive(Debug, Deserialize)]
  pub struct Config {
      pub feldspar: FeldsparConfig,
      pub trace_review: TraceReviewConfig,
      pub thresholds: ThresholdsConfig,
      pub budgets: HashMap<String, [u32; 2]>,
      pub pruning: PruningConfig,
      pub modes: HashMap<String, ModeConfig>,
      pub components: ComponentsConfig,
      #[serde(skip)]
      pub principles: Vec<PrincipleGroup>,
  }

  #[derive(Debug, Deserialize)]
  pub struct FeldsparConfig {
      pub db_path: String,
      pub model_path: String,
      pub recap_every: u32,
  }

  #[derive(Debug, Deserialize)]
  pub struct TraceReviewConfig {
      pub api_key_env: String,
      pub model: String,
  }

  #[derive(Debug, Deserialize)]
  pub struct ThresholdsConfig {
      pub confidence_gap: f64,
      pub over_analysis_multiplier: f64,
      pub overthinking_multiplier: f64,
  }

  #[derive(Debug, Deserialize)]
  pub struct PruningConfig {
      pub no_outcome_days: u32,
      pub low_quality_days: u32,
      pub with_outcome_days: u32,
  }

  #[derive(Debug, Deserialize)]
  pub struct ModeConfig {
      pub requires: Vec<String>,
      pub budget: String,
      pub watches: String,
  }

  #[derive(Debug, Deserialize)]
  pub struct ComponentsConfig {
      pub valid: Vec<String>,
  }
  ```
- **Code — Principles types (two-stage)**:
  ```rust
  // Stage 1: raw YAML parse target
  #[derive(Debug, Deserialize)]
  struct RawPrinciples {
      groups: HashMap<String, RawPrincipleGroup>,
  }

  #[derive(Debug, Deserialize)]
  struct RawPrincipleGroup {
      #[serde(default)]
      active: bool,
      principles: Vec<Principle>,
  }

  // Stage 2: final types (map key injected as name)
  #[derive(Debug, Clone)]
  pub struct PrincipleGroup {
      pub name: String,
      pub active: bool,
      pub principles: Vec<Principle>,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct Principle {
      pub name: String,
      pub rule: String,
      #[serde(default)]
      pub ask: Vec<String>,
  }
  ```
- **Code — Config::load()**:
  ```rust
  impl Config {
      pub fn load(toml_path: &str, principles_path: &str) -> Arc<Config> {
          let toml_str = std::fs::read_to_string(toml_path)
              .unwrap_or_else(|e| panic!("failed to read config '{}': {}", toml_path, e));
          let mut config: Config = toml::from_str(&toml_str)
              .unwrap_or_else(|e| panic!("failed to parse config '{}': {}", toml_path, e));

          let principles = load_principles(principles_path);
          validate(&config, &principles);
          config.principles = principles;
          Arc::new(config)
      }
  }
  ```
- **Code — load_principles()**:
  ```rust
  fn load_principles(path: &str) -> Vec<PrincipleGroup> {
      let yaml = std::fs::read_to_string(path)
          .unwrap_or_else(|e| panic!("failed to read principles file '{}': {}", path, e));

      let raw: RawPrinciples = serde_saphyr::from_str(&yaml)
          .unwrap_or_else(|e| panic!("failed to parse principles YAML '{}': {}", path, e));

      raw.groups
          .into_iter()
          .filter(|(_, group)| group.active)
          .map(|(name, group)| PrincipleGroup {
              name,
              active: group.active,
              principles: group.principles,
          })
          .collect()
  }
  ```
- **Code — validate()**:
  ```rust
  fn validate(config: &Config, principles: &[PrincipleGroup]) {
      // Budget ranges: min <= max
      for (name, range) in &config.budgets {
          assert!(
              range[0] <= range[1],
              "budget '{}' has min > max: [{}, {}]",
              name, range[0], range[1]
          );
      }

      // Modes: budget tier exists, requires values are valid
      for (name, mode) in &config.modes {
          assert!(
              config.budgets.contains_key(&mode.budget),
              "mode '{}' references unknown budget tier '{}'",
              name, mode.budget
          );
          for req in &mode.requires {
              assert!(
                  VALID_REQUIRES.contains(&req.as_str()),
                  "mode '{}' requires unknown field '{}'. Valid: {}",
                  name, req, VALID_REQUIRES.join(", ")
              );
          }
      }

      // Numeric sanity
      assert!(config.feldspar.recap_every > 0, "recap_every must be > 0");
      assert!(config.pruning.no_outcome_days > 0, "pruning.no_outcome_days must be > 0");
      assert!(config.pruning.low_quality_days > 0, "pruning.low_quality_days must be > 0");
      assert!(config.pruning.with_outcome_days > 0, "pruning.with_outcome_days must be > 0");
      assert!(config.thresholds.confidence_gap > 0.0, "thresholds.confidence_gap must be > 0");
      assert!(config.thresholds.over_analysis_multiplier > 0.0, "thresholds.over_analysis_multiplier must be > 0");
      assert!(config.thresholds.overthinking_multiplier > 0.0, "thresholds.overthinking_multiplier must be > 0");

      // Principles: active groups must have at least one principle
      for group in principles {
          assert!(
              !group.principles.is_empty(),
              "principle group '{}' is active but has no principles",
              group.name
          );
      }
  }
  ```

---

### Task 3: Implement thought types in src/thought.rs

- **Description**: Replace the comment stub in `src/thought.rs` with all type definitions: ThoughtInput, ThoughtResult, ThoughtRecord, Impact, Alert, Severity, Trace, ThinkingServer, DbPool, MlModel, Timestamp.
- **Acceptance Criteria**:
  - [ ] All types defined with exact derives from the design doc
  - [ ] ThoughtInput: `Debug, Clone, Deserialize` with `rename_all = "camelCase"` and `#[serde(default)]` on optional collection/bool fields
  - [ ] ThoughtResult: `Debug, Serialize, Deserialize, Default` with `rename_all = "camelCase"`
  - [ ] Impact: `Debug, Serialize, Deserialize, Default, Clone`
  - [ ] Alert: `Debug, Serialize, Deserialize, Clone`
  - [ ] Severity: `Debug, Serialize, Deserialize, Clone, PartialEq, Eq`
  - [ ] ThinkingServer has `new(config: Arc<Config>)` constructor
  - [ ] DbPool and MlModel are empty marker structs
  - [ ] `cargo check` passes
- **Files to Create/Modify**:
  ```
  src/thought.rs  ← replace stub entirely
  ```
- **Dependencies**: Task 2 (config.rs must exist for `use crate::config::Config`)
- **Code — Full thought.rs**:
  ```rust
  use crate::config::Config;
  use serde::{Deserialize, Serialize};
  use std::collections::HashMap;
  use std::sync::Arc;
  use tokio::sync::RwLock;

  pub type Timestamp = i64;

  #[derive(Debug, Clone, Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct ThoughtInput {
      pub thought: String,
      pub thought_number: u32,
      pub total_thoughts: u32,
      pub next_thought_needed: bool,
      pub thinking_mode: Option<String>,
      #[serde(default)]
      pub affected_components: Vec<String>,
      pub confidence: Option<f64>,
      #[serde(default)]
      pub evidence: Vec<String>,
      pub estimated_impact: Option<Impact>,
      #[serde(default)]
      pub is_revision: bool,
      pub revises_thought: Option<u32>,
      pub branch_from_thought: Option<u32>,
      pub branch_id: Option<String>,
      #[serde(default)]
      pub needs_more_thoughts: bool,
  }

  #[derive(Debug, Serialize, Deserialize, Default)]
  #[serde(rename_all = "camelCase")]
  pub struct ThoughtResult {
      pub warnings: Vec<String>,
      pub alerts: Vec<Alert>,
      pub confidence_calculated: Option<f64>,
      pub depth_overlap: Option<f64>,
      pub budget_used: u32,
      pub budget_max: u32,
      pub budget_category: String,
      pub ml_trajectory: Option<f64>,
      pub ml_drift: Option<bool>,
      pub recap: Option<String>,
      pub adr: Option<String>,
      pub auto_outcome: Option<f64>,
  }

  #[derive(Debug, Serialize, Deserialize, Default, Clone)]
  pub struct Impact {
      pub latency: Option<String>,
      pub throughput: Option<String>,
      pub risk: Option<String>,
  }

  #[derive(Debug, Serialize, Deserialize, Clone)]
  pub struct Alert {
      pub analyzer: String,
      pub kind: String,
      pub severity: Severity,
      pub message: String,
  }

  #[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
  pub enum Severity {
      Medium,
      High,
  }

  pub struct ThoughtRecord {
      pub input: ThoughtInput,
      pub result: ThoughtResult,
      pub created_at: Timestamp,
  }

  pub struct Trace {
      pub id: String,
      pub thoughts: Vec<ThoughtRecord>,
      pub created_at: Timestamp,
      pub closed: bool,
  }

  pub struct DbPool;
  pub struct MlModel;

  pub struct ThinkingServer {
      pub traces: RwLock<HashMap<String, Trace>>,
      pub config: Arc<Config>,
      pub db: Option<DbPool>,
      pub ml: Option<MlModel>,
  }

  impl ThinkingServer {
      pub fn new(config: Arc<Config>) -> Self {
          Self {
              traces: RwLock::new(HashMap::new()),
              config,
              db: None,
              ml: None,
          }
      }
  }
  ```

---

### Task 4: Write config tests in src/config.rs

- **Description**: Add `#[cfg(test)] mod tests` at the bottom of `src/config.rs` with all config-related tests.
- **Acceptance Criteria**:
  - [ ] All 10 test cases listed below are implemented and passing
  - [ ] Tests use the real `config/feldspar.toml` and `config/principles.yaml` files where noted
  - [ ] Panic tests use `#[should_panic(expected = "...")]` with the exact panic message substring
  - [ ] `cargo test --lib config` passes
- **Files to Modify**:
  ```
  src/config.rs  ← append #[cfg(test)] mod tests block
  ```
- **Dependencies**: Task 2
- **Test Cases** (file: `src/config.rs` inline `#[cfg(test)]`):

  - **`test_valid_config_parses`**: Call `Config::load("config/feldspar.toml", "config/principles.yaml")`. Assert: `config.feldspar.db_path == "feldspar.db"`, `config.feldspar.recap_every == 3`, `config.modes.contains_key("architecture")`, `config.budgets.contains_key("deep")`, `config.thresholds.confidence_gap == 25.0`.

  - **`test_principles_load`**: Call `Config::load(...)`. Assert: `!config.principles.is_empty()`. Assert that at least one principle group with `name == "solid"` exists. Assert that group has non-empty `principles` vec.

  - **`test_principles_key_to_name`**: Call `Config::load(...)`. Collect all `PrincipleGroup.name` values into a `Vec<String>`. Assert contains `"solid"` and `"kiss-dry"` (both active in the YAML). Assert does NOT contain `"tdd"` or `"security"` (both `active: false`).

  - **`test_inactive_groups_excluded`**: Call `Config::load(...)`. Assert no group with `name == "tdd"` exists (it's `active: false` in the YAML).

  - **`test_invalid_toml_panics`**: Write a helper that calls `toml::from_str::<Config>("not valid toml {{{{")`. Use `#[should_panic]`. (Cannot use `Config::load` since it reads a file — test the parse stage directly.)

  - **`test_unknown_budget_tier_panics`**: Build a valid `Config` in memory, set one mode's `budget` to `"nonexistent"`, call `validate(&config, &principles)`. `#[should_panic(expected = "unknown budget tier")]`.

  - **`test_budget_min_gt_max_panics`**: Build a valid `Config` in memory, set `budgets.insert("bad", [5, 2])`, call `validate(...)`. `#[should_panic(expected = "has min > max")]`.

  - **`test_recap_every_zero_panics`**: Build a valid `Config` in memory with `recap_every = 0`, call `validate(...)`. `#[should_panic(expected = "recap_every must be > 0")]`.

  - **`test_empty_active_group_panics`**: Call `validate()` with a `PrincipleGroup { name: "empty".into(), active: true, principles: vec![] }`. `#[should_panic(expected = "active but has no principles")]`.

  - **`test_unknown_requires_panics`**: Build a valid `Config` in memory, set one mode's `requires` to `vec!["nonexistent".into()]`, call `validate(...)`. `#[should_panic(expected = "requires unknown field")]`.

  - **Helper**: Create a `fn test_config() -> Config` helper that builds a minimal valid Config programmatically (not from file) for the panic tests to mutate. This avoids coupling panic tests to the real TOML file.
    ```rust
    fn test_config() -> Config {
        Config {
            feldspar: FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
            },
            trace_review: TraceReviewConfig {
                api_key_env: "TEST_KEY".into(),
                model: "test-model".into(),
            },
            thresholds: ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([
                ("minimal".into(), [2, 3]),
                ("standard".into(), [3, 5]),
                ("deep".into(), [5, 8]),
            ]),
            pruning: PruningConfig {
                no_outcome_days: 30,
                low_quality_days: 15,
                with_outcome_days: 90,
            },
            modes: HashMap::from([(
                "test-mode".into(),
                ModeConfig {
                    requires: vec![],
                    budget: "standard".into(),
                    watches: "test watches".into(),
                },
            )]),
            components: ComponentsConfig { valid: vec![] },
            principles: vec![],
        }
    }
    ```

---

### Task 5: Write thought type tests in src/thought.rs

- **Description**: Add `#[cfg(test)] mod tests` at the bottom of `src/thought.rs` with serde round-trip and construction tests.
- **Acceptance Criteria**:
  - [ ] All 5 test cases listed below are implemented and passing
  - [ ] `cargo test --lib thought` passes
- **Files to Modify**:
  ```
  src/thought.rs  ← append #[cfg(test)] mod tests block
  ```
- **Dependencies**: Task 3
- **Test Cases** (file: `src/thought.rs` inline `#[cfg(test)]`):

  - **`test_thought_input_deserialize`**: Deserialize this JSON into `ThoughtInput`:
    ```rust
    let json = r#"{
        "thought": "Analyzing the auth flow",
        "thoughtNumber": 1,
        "totalThoughts": 5,
        "nextThoughtNeeded": true,
        "thinkingMode": "architecture",
        "affectedComponents": ["auth", "sessions"],
        "confidence": 75.0,
        "evidence": ["src/auth.rs"],
        "isRevision": false,
        "needsMoreThoughts": false
    }"#;
    let input: ThoughtInput = serde_json::from_str(json).unwrap();
    ```
    Assert: `input.thought == "Analyzing the auth flow"`, `input.thought_number == 1`, `input.next_thought_needed == true`, `input.thinking_mode == Some("architecture".into())`, `input.affected_components.len() == 2`, `input.confidence == Some(75.0)`.

  - **`test_thought_input_defaults`**: Deserialize minimal JSON (only required fields):
    ```rust
    let json = r#"{
        "thought": "Quick check",
        "thoughtNumber": 1,
        "totalThoughts": 1,
        "nextThoughtNeeded": false
    }"#;
    let input: ThoughtInput = serde_json::from_str(json).unwrap();
    ```
    Assert: `input.affected_components.is_empty()`, `input.evidence.is_empty()`, `input.is_revision == false`, `input.needs_more_thoughts == false`, `input.confidence.is_none()`, `input.thinking_mode.is_none()`.

  - **`test_thought_result_serialize`**: Serialize a `ThoughtResult::default()` to JSON. Parse the JSON string with `serde_json::Value`. Assert the keys use camelCase: `value["budgetUsed"]` exists, `value["mlTrajectory"]` exists, `value["confidenceCalculated"]` exists. Assert `value["budgetUsed"] == 0`, `value["warnings"]` is an empty array.

  - **`test_impact_default`**: `let impact = Impact::default();` Assert: `impact.latency.is_none()`, `impact.throughput.is_none()`, `impact.risk.is_none()`.

  - **`test_server_new`**: Build a `Config` using `Config::load("config/feldspar.toml", "config/principles.yaml")`, create `ThinkingServer::new(config)`. Assert: `server.db.is_none()`, `server.ml.is_none()`. Use `server.traces.read().await` to assert the HashMap is empty (this test needs `#[tokio::test]`).

---

## Testing Strategy

- **Framework**: Rust built-in `#[cfg(test)]` with `cargo test`
- **Structure**: Tests inline in each source file (`src/config.rs`, `src/thought.rs`)
- **Coverage targets**: All 15 test cases passing, covering: valid parse, all 6 validation panic cases, principles two-stage loading, serde round-trips, defaults, server construction
- **Run command**: `cargo test` (runs all), or `cargo test --lib config` / `cargo test --lib thought` for targeted

---

## Risk Mitigation

| Risk | Probability | Impact | Mitigation | Fallback | Detection |
|------|------------|--------|------------|----------|-----------|
| `serde-saphyr` API differs from `serde_yaml` | Low | Medium | API is serde-compatible (`from_str` works identically) | Check docs at `docs.rs/serde-saphyr` | `cargo check` after Task 1 |
| TOML HashMap deserialization of `[modes.*]` fails | Low | High | `toml` crate handles `HashMap<String, T>` for TOML tables natively | Test with real `feldspar.toml` in Task 4 | `test_valid_config_parses` |
| `#[serde(skip)]` on `principles` field fails without `Default` for `Vec` | Very Low | Medium | `Vec<T>` implements `Default` (empty vec) — this works | Use `#[serde(skip_deserializing, default)]` for explicitness | `cargo check` |
| Principles YAML structure changes | Low | Medium | Tests validate against real `config/principles.yaml` | Two-stage parse is resilient to group additions/removals | `test_principles_load`, `test_principles_key_to_name` |
| Validation panics give unclear errors | Low | Low | All panic messages include the offending name/value | Collect all errors into Vec<String> and panic once (future improvement) | Manual review of panic output |

---

## Success Criteria

### Functional Requirements
- [ ] `Config::load("config/feldspar.toml", "config/principles.yaml")` returns a fully populated `Arc<Config>`
- [ ] All 8 thinking modes from `feldspar.toml` are in `config.modes`
- [ ] Active principle groups from `principles.yaml` are loaded with correct names
- [ ] Invalid config panics with descriptive error message
- [ ] `ThinkingServer::new(config)` creates server with empty traces, db None, ml None
- [ ] ThoughtInput deserializes from camelCase MCP JSON
- [ ] ThoughtResult serializes to camelCase JSON

### Non-Functional Requirements
- [ ] All 15 tests passing (`cargo test`)
- [ ] `cargo clippy` produces no warnings on new code
- [ ] No unused imports or dead code warnings
- [ ] All pub types have `Debug` derive

---

## Implementation Notes

- **Existing stubs**: `src/thought.rs` and `src/config.rs` currently contain comment stubs only. Replace them entirely — don't try to preserve the comments.
- **Module declarations**: `src/main.rs` already has `mod config;` and `mod thought;` declared. Don't modify `main.rs`.
- **Other module stubs**: `src/main.rs` also declares `mod analyzers; mod db; mod ml; mod pruning; mod trace_review; mod warnings;` — these are still stubs. Don't touch them. Your code should compile alongside them (they're empty modules).
- **Config file paths in tests**: Use relative paths `"config/feldspar.toml"` and `"config/principles.yaml"` — `cargo test` runs from the project root.
- **`serde-saphyr` import**: The crate is imported as `serde_saphyr` in Rust code (hyphens become underscores).
- **Edition 2024**: The project uses Rust edition 2024 (requires Rust 1.85+). This affects import syntax — `use crate::config::Config;` is the correct form.
- **No `#[allow(...)]` attributes**: Don't suppress warnings. Fix them.
