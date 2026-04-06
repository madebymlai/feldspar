# Solution Design: Types + Config

## 1. Executive Summary

Define all core Rust types and the TOML/YAML config loader for feldspar. This is the foundation layer — every other module imports these types and reads this config. Deliverables: `src/thought.rs` (types), `src/config.rs` (config loader), `serde-saphyr` dependency addition, and tests proving parse/validation correctness.

## 2. Rationale

| Decision | Rationale | Alternative | Why Rejected |
|----------|-----------|-------------|--------------|
| `i64` unix millis for timestamps | Maps directly to SQLite INTEGER. No extra dependency. Simple type alias. | `chrono::DateTime<Utc>` | Adds dependency for no benefit at this layer |
| `serde-saphyr` for YAML | `serde_yaml` deprecated March 2024, repo archived. serde-saphyr is actively maintained, panic-free, faster, 1000+ tests. | `serde_yaml` | Unmaintained, archived repo |
| Empty marker types for db/ml | `DbPool` and `MlModel` as empty structs. Clean swap when issues #5/#6 land. | Trait objects (`Box<dyn Db>`) | KISS — no abstraction for what doesn't exist yet |
| `Option<DbPool>`, `Option<MlModel>` | Server can run without db/ml until wired. Explicit about optionality. | Owned empty structs (always present) | Hides the "not yet implemented" state |
| Two-stage config parse | Stage 1: serde parses TOML/YAML into raw structs. Stage 2: semantic validation cross-references (budget tiers, mode requires). Clean separation. | `deny_unknown_fields` on everything | Conflicts with `HashMap`-based dynamic modes section |
| `rename_all = "camelCase"` on wire types | MCP JSON uses camelCase (`thoughtNumber`, `nextThoughtNeeded`). Rust uses snake_case internally. | Manual `#[serde(rename)]` per field | Boilerplate explosion, error-prone |
| `RwLock<HashMap>` for traces | Reads (per-thought lookup) far outnumber writes (trace create/evict). No new dependency — `tokio::sync::RwLock`. | `DashMap` | Extra dependency for something that's not a hot path |
| `Arc<Config>` sharing | Loaded once at startup, immutable forever. Cloned to handlers. | `&Config` with lifetimes | Threading lifetimes through async handlers is painful |
| Full fidelity principles | `Vec<PrincipleGroup>` preserving group structure, active flag, principle name/rule/ask. | Flattened `Vec<String>` | Downstream consumers (reviewers, agents) need the full structure |

## 3. Technology Stack

| Component | Crate | Version | Purpose |
|-----------|-------|---------|---------|
| Serialization | `serde` | 1 | Derive Serialize/Deserialize |
| JSON | `serde_json` | 1 | MCP wire format |
| TOML | `toml` | 0.8 | Config file parsing |
| YAML | `serde-saphyr` | latest | Principles file parsing (replaces deprecated serde_yaml) |
| UUID | `uuid` | 1 (v4) | Trace ID generation |
| Async lock | `tokio::sync::RwLock` | (from tokio 1) | Concurrent trace access |

**Cargo.toml addition:**
```toml
serde-saphyr = "1"
```

## 4. Architecture

### Data Flow

```
Config::load("config/feldspar.toml", "config/principles.yaml")
  → read TOML file → serde parse into RawConfig
  → read YAML file → serde parse into RawPrinciples
  → validate(raw_config, raw_principles) → panics on invalid
  → return Config (immutable)

ThinkingServer::new(Arc<Config>)
  → empty traces HashMap, db: None, ml: None
  → ready to accept thoughts (processing logic in later issues)
```

### Module Catalog

**`src/thought.rs`** — All type definitions

| Type | Role | Serde |
|------|------|-------|
| `ThoughtInput` | Wire input from Claude MCP JSON | `Deserialize`, `rename_all = "camelCase"` |
| `ThoughtResult` | Wire output + DB storage | `Serialize`, `Deserialize`, `rename_all = "camelCase"`, `Default` |
| `ThoughtRecord` | Input + result + timestamp | Internal only |
| `Impact` | Latency/throughput/risk estimates | `Serialize`, `Deserialize`, `Default` |
| `Alert` | Single analyzer finding | `Serialize`, `Deserialize`, `Clone` |
| `Severity` | `Medium` \| `High` closed enum | `Serialize`, `Deserialize`, `Clone`, `PartialEq`, `Eq` |
| `Trace` | Reasoning chain container | Internal only |
| `ThinkingServer` | Top-level concurrent state | Internal only |
| `DbPool` | Empty marker (issue #5 replaces) | None |
| `MlModel` | Empty marker (issue #6 replaces) | None |

**`src/config.rs`** — Config loader and validator

| Type | Role |
|------|------|
| `Config` | Final validated config, wrapped in `Arc` for sharing |
| `TraceReviewConfig` | `api_key_env`, `model` |
| `ThresholdsConfig` | `confidence_gap`, `over_analysis_multiplier`, `overthinking_multiplier` |
| `PruningConfig` | Retention periods |
| `ModeConfig` | `requires`, `budget`, `watches` per thinking mode |
| `RawPrinciples` | YAML root — `groups: HashMap<String, RawPrincipleGroup>` |
| `RawPrincipleGroup` | Pre-mapping — `active`, `principles` (no name yet) |
| `PrincipleGroup` | Final — group name (from map key), active flag, `Vec<Principle>` |
| `Principle` | `name`, `rule`, `ask: Vec<String>` |

## 5. Protocol/Schema

### ThoughtInput (Deserialize, camelCase)

```rust
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
```

### ThoughtResult (Serialize, camelCase)

```rust
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
```

### Supporting Types

```rust
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

// Placeholder types — replaced by issues #5 and #6
pub struct DbPool;
pub struct MlModel;

pub struct ThinkingServer {
    pub traces: tokio::sync::RwLock<std::collections::HashMap<String, Trace>>,
    pub config: std::sync::Arc<Config>,
    pub db: Option<DbPool>,
    pub ml: Option<MlModel>,
}
```

### Config Schema

```rust
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
    pub principles: Vec<PrincipleGroup>,  // loaded separately from YAML
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

// Principles (from YAML) — two-stage deserialization
// Stage 1: serde parses YAML into raw types (map keys are group names)
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

// Stage 2: mapped into final types by load_principles()
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

### Config::load() entry point

```rust
impl Config {
    pub fn load(toml_path: &str, principles_path: &str) -> Arc<Config> {
        // Stage 1: Parse
        let raw: Config = toml::from_str(&std::fs::read_to_string(toml_path).expect("...")).expect("...");
        let principles = load_principles(principles_path);

        // Stage 2: Validate
        validate(&raw, &principles);

        // Stage 3: Freeze
        let mut config = raw;
        config.principles = principles;
        Arc::new(config)
    }
}
```

### load_principles()

```rust
/// Two-stage YAML loading: parse into raw HashMap, then map keys to PrincipleGroup names.
/// Filters to active groups only. Validates non-empty principles per active group.
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

### Semantic Validation Rules

Valid `mode.requires` values (closed set): `["components", "evidence", "latency", "confidence"]`

| Check | Panic message |
|-------|---------------|
| Mode budget references nonexistent tier | `"mode '{name}' references unknown budget tier '{tier}'"` |
| Mode requires contains unknown value | `"mode '{name}' requires unknown field '{field}'. Valid: components, evidence, latency, confidence"` |
| Budget range min > max | `"budget '{name}' has min > max: [{min}, {max}]"` |
| `recap_every` is 0 | `"recap_every must be > 0"` |
| Pruning days are 0 | `"pruning.{field} must be > 0"` |
| Thresholds are negative | `"thresholds.{field} must be > 0"` |
| Active principle group has no principles | `"principle group '{name}' is active but has no principles"` |

## 6. Implementation Details

### File Structure

```
src/thought.rs    ← ThoughtInput, ThoughtResult, ThoughtRecord, Impact, Alert,
                    Severity, Trace, ThinkingServer, DbPool, MlModel, Timestamp
src/config.rs     ← Config, all sub-configs, PrincipleGroup, Principle,
                    Config::load(), validate(), load_principles()
Cargo.toml        ← add serde-saphyr dependency
```

### Integration Points

- `ThinkingServer::new(config: Arc<Config>)` — called from `src/main.rs` at startup
- `Config::load(toml_path, principles_path)` — called from `src/main.rs` before server construction
- All types in `thought.rs` are `pub` — imported by `analyzers/`, `warnings.rs`, `ml.rs`, `db.rs`
- `Alert` and `Severity` are used by analyzer traits (issue #2)
- `ThoughtInput` signature matches MCP JSON-RPC params (issue #3 wires it)
- **Response assembly (issue #3 responsibility)**: The flat wire response is NOT ThoughtResult directly. Issue #3 builds it by merging: (1) echo-back fields from ThoughtInput (`thoughtNumber`, `totalThoughts`, `nextThoughtNeeded`), (2) Trace metadata (`traceId`, `branches`, `thoughtHistoryLength`), and (3) ThoughtResult fields (renamed where wire format differs: `ml_trajectory` → `trajectory`, `ml_drift` → `driftDetected`, `confidence_calculated` → `confidenceCalculated` + derived `confidenceGap`). ThoughtResult is the internal computed representation; the wire format is a superset assembled at the handler level.

### Issues that update this code later

| Issue | What changes |
|-------|-------------|
| #5 (DB) | Replace `DbPool` marker with `rusqlite::Connection` wrapper, change `Option<DbPool>` type |
| #6 (ML) | Replace `MlModel` marker with `PerpetualBooster` wrapper, change `Option<MlModel>` type |
| #3 (MCP server) | Add methods to `ThinkingServer` for thought processing |
| #2 (Analyzers) | Import `Alert`, `Severity`, `ThoughtInput`, `ThoughtRecord`, `Config` |

### Test Plan

```rust
#[cfg(test)]
mod tests {
    // Config parsing
    - test_valid_config_parses: load config/feldspar.toml, assert all fields populated
    - test_invalid_toml_panics: malformed TOML string, expect panic
    - test_unknown_budget_tier_panics: mode references "nonexistent" tier
    - test_budget_min_gt_max_panics: budget = [5, 2]
    - test_recap_every_zero_panics: recap_every = 0
    - test_principles_load: load config/principles.yaml, assert active groups parsed with correct names
    - test_principles_key_to_name: verify YAML map keys become PrincipleGroup.name values
    - test_inactive_groups_excluded: only active groups in final config
    - test_empty_active_group_panics: active group with no principles
    - test_unknown_requires_panics: mode with requires = ["nonexistent"]

    // Type serialization
    - test_thought_input_deserialize: JSON with camelCase → ThoughtInput
    - test_thought_result_serialize: ThoughtResult → JSON with camelCase
    - test_thought_input_defaults: missing optional fields get defaults
    - test_impact_default: Default::default() gives all None

    // ThinkingServer
    - test_server_new: construct with config, verify empty traces, db None, ml None
}
```
