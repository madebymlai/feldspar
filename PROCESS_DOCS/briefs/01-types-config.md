# Brief: Types + Config

**Problem**: Every feldspar module depends on core data structures and configuration. Nothing compiles without these. This is the foundation layer.

**Requirements**:
- Define all core Rust types in `src/thought.rs`: ThoughtInput (Deserialize), ThoughtResult (Serialize), ThoughtRecord, Impact, Alert, Severity, Trace, ThinkingServer
- All struct fields match the design doc exactly (see `docs/plans/2026-04-06-thought-processor-design.md`, Types section)
- ThinkingServer holds `db: Option<DbPool>`, `ml: Option<MlModel>` — None until issues #5/#6 wire real implementations
- Define Config struct in `src/config.rs` that parses `config/feldspar.toml`
- Load `config/principles.yaml` into Config — active groups only, stored for later consumers (no logic in this issue)
- Full semantic validation at startup: parse errors, cross-reference checks (budget tiers exist, required fields valid), panic on invalid

**Constraints**:
- Input/output separation: ThoughtInput is Deserialize only, ThoughtResult is Serialize only
- ThinkingMode is `Option<String>` validated against config at runtime, not a Rust enum
- Branching is flat with tags (`branch_id: Option<String>`), not a tree
- Severity is a closed enum (`Medium | High`), not configurable
- No trait abstractions for db/ml — use Option-wrapped concrete types, swap later

**Non-goals**:
- No analyzer logic, no ML logic, no DB logic — just the types they'll use
- No runtime behavior beyond config loading and validation
- No principles enforcement logic — just parsing and storing them
- No ThinkingServer methods beyond construction

**Style**: Minimal, type-correct, zero runtime behavior. If it compiles with all fields populated, it's done.

**Key concepts**:
- **ThoughtInput**: What Claude sends via MCP JSON. Raw input only.
- **ThoughtResult**: What feldspar computes. Warnings, alerts, ML signals, recap, ADR.
- **ThoughtRecord**: ThoughtInput + ThoughtResult + timestamp. Stored in Trace.
- **Trace**: A reasoning chain. UUID-keyed, flat vec of records, branches tagged not nested.
- **ThinkingServer**: The top-level struct. Holds all active traces, db, ml, pruner.
- **Semantic validation**: Config validation beyond parsing — e.g. mode references budget tier "deep", verify "deep" exists in `[budgets]`.
