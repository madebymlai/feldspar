# Thought Processor Design

Date: 2026-04-06

---

## Core Model

```
ThinkingServer
├── traces: HashMap<String, Trace>    // concurrent traces, keyed by UUID
├── db: SqlitePool                     // shared, async writes, single file
├── ml: PerpetualBooster               // shared, in-process Rust crate
└── pruner: Pruner                     // periodic cleanup
```

```
Trace
├── id: String                         // UUID, feldspar-generated on thoughtNumber=1
├── thoughts: Vec<ThoughtRecord>       // flat vec, branches tagged not nested
├── created_at: Timestamp
└── closed: bool
```

### Type Design (locked in)

Input and computed outputs are separate structs. Input is Deserialize (from Claude).
Result is Serialize (to response + DB). Record combines both for storage.

```rust
/// What Claude sends (Deserialize from MCP JSON)
#[derive(Deserialize)]
struct ThoughtInput {
    thought: String,
    thought_number: u32,
    total_thoughts: u32,
    next_thought_needed: bool,
    thinking_mode: Option<String>,        // validated against config at runtime, not an enum
    affected_components: Vec<String>,
    confidence: Option<f64>,              // 0-100, self-reported
    evidence: Vec<String>,               // file paths, docs, measurements
    estimated_impact: Option<Impact>,
    is_revision: bool,
    revises_thought: Option<u32>,
    branch_from_thought: Option<u32>,
    branch_id: Option<String>,           // None = main line
    needs_more_thoughts: bool,
}

/// Impact estimate (all strings -- users write "~200ms" not numbers)
#[derive(Serialize, Deserialize, Default)]
struct Impact {
    latency: Option<String>,
    throughput: Option<String>,
    risk: Option<String>,
}

/// What feldspar computes (Serialize to response + DB)
#[derive(Serialize, Default)]
struct ThoughtResult {
    warnings: Vec<String>,
    alerts: Vec<Alert>,
    confidence_calculated: Option<f64>,
    depth_overlap: Option<f64>,
    budget_used: u32,
    budget_max: u32,
    budget_category: String,
    ml_trajectory: Option<f64>,          // 0-1 trajectory score
    ml_drift: Option<bool>,
    recap: Option<String>,               // every N thoughts, scoped to current branch
    adr: Option<String>,                 // only on completion
    auto_outcome: Option<f64>,           // only on completion
}

/// Single analyzer alert
#[derive(Serialize)]
struct Alert {
    analyzer: String,                    // "confidence", "sycophancy", "bias", etc.
    kind: String,                        // "OVERCONFIDENCE", "confirmation_only", etc.
    severity: Severity,
    message: String,
}

/// Closed set -- not user-configurable
#[derive(Serialize)]
enum Severity {
    Medium,
    High,
}

/// Stored in trace -- combines input + computed
struct ThoughtRecord {
    input: ThoughtInput,
    result: ThoughtResult,
    created_at: Timestamp,
}
```

### Thinking Modes (runtime-configured, not compile-time enum)

Modes defined in config, each specifies required fields, budget tier, and warning rules:

```toml
[modes.architecture]
requires = ["components"]
budget = "deep"

[modes.debugging]
requires = ["evidence"]
budget = "standard"

[modes.code-review]
requires = ["evidence", "components"]
budget = "standard"
```

New mode = add a block to config. No recompile. Agents can use custom modes.

---

## Trace Lifecycle

### Creation
- First thought with `thoughtNumber: 1` creates a new Trace
- Feldspar generates UUID, returns it in response as `traceId`
- Claude passes `traceId` on all subsequent thoughts

### Per Thought
```
thought arrives with traceId
  → parse into ThoughtData (input fields)
  → append to trace.thoughts
  → run analyzer pipeline (fills computed fields)
  → run ml.predict + ml.drift (fills ml fields)
  → run warning engine (fills warnings)
  → generate recap if thought_number % N == 0 (scoped to current branch)
  → build flat JSON response
  → async db.write_thought (best-effort, never blocks response)
  → return response to Claude
```

### Completion
```
nextThoughtNeeded = false
  → generate ADR skeleton from trace
  → compute auto-outcome from analyzer outputs (0-1)
  → ml.train(features, auto_outcome)
  → db.flush_trace(trace)
  → evict trace from memory
```

### Concurrent Traces
- Multiple traces active simultaneously (parallel agents, multiple conversations)
- Each trace is independent: own thoughts, own branches, own analyzer state
- All traces share DB and ML model
- No interaction between traces

---

## Branching

Flat vec with tags, not tree structure.

- `branch_id: None` = main line
- `branch_id: Some("alt-sessions")` = alternative approach
- `branch_from_thought: Some(2)` = forked from thought #2
- Analyzers filter by `branch_id` when comparing consecutive thoughts
- Recap scoped to current branch (only summarizes thoughts on same branch_id)

---

## Analyzer Pipeline

Observer/Evaluator pipeline. Two traits, two phases. Parallel within each phase (rayon).
No duplication of logic (DRY). Observers produce raw signals, Evaluators reason over them.

```
Observers (parallel, no cross-deps):
  ├── depth      → Observation::Depth { overlap, contradictions }
  ├── budget     → Observation::Budget { used, max, category }
  └── bias       → Observation::Bias { detected }

        ↓ merge into Observations struct

Evaluators (parallel, read Observations):
  ├── confidence → uses bias_detected for bias avoidance score (0-10 pts)
  └── sycophancy → uses depth_overlap for confirmation-only detection
```

Both Observers and Evaluators can produce alerts. All alerts merged into final output.

### Traits (locked in)

```rust
/// Observers: run first, produce raw signals independently
trait Observer {
    fn observe(&self, input: &ThoughtInput, trace: &[ThoughtRecord], config: &Config) -> Observation;
}

/// Evaluators: run second, reason over observer signals
trait Evaluator {
    fn evaluate(&self, input: &ThoughtInput, trace: &[ThoughtRecord], observations: &Observations, config: &Config) -> Option<Alert>;
}

/// What each observer produces
enum Observation {
    Depth { overlap: Option<f64>, contradictions: Vec<(u32, u32)> },
    Bias { detected: Option<String> },
    Budget { used: u32, max: u32, category: String },
}

/// Merged observer outputs, passed to evaluators
struct Observations {
    depth_overlap: Option<f64>,
    contradictions: Vec<(u32, u32)>,
    bias_detected: Option<String>,
    budget_used: u32,
    budget_max: u32,
    budget_category: String,
}

/// Pipeline runner
fn run_pipeline(input: &ThoughtInput, trace: &[ThoughtRecord], config: &Config) -> (Vec<Alert>, Observations) {
    // Observers: parallel via rayon
    let observations = observe_all(observers, input, trace, config);

    // Evaluators: parallel via rayon, read observations
    let eval_alerts = evaluate_all(evaluators, input, trace, &observations, config);

    // Merge observer alerts + evaluator alerts
    (all_alerts, observations)
}
```

### Error handling
Analyzers never fail. They return Option<Alert> -- None means no issue found.
If an analyzer panics (bug), catch_unwind logs it and skips that analyzer.
One broken analyzer doesn't take down the pipeline.

---

## ML: Per-Thought Real-Time Inference

### Feature Vector (rolling window)

Per-thought snapshot + trace aggregates so far:

```
// Current thought
thinking_mode: categorical
component_count: u32
confidence_reported: f64
evidence_count: u32
has_branch: bool
has_revision: bool
warning_count: u32

// Trace aggregates (rolling)
avg_confidence: f64
avg_confidence_gap: f64
total_biases_detected: u32
total_sycophancy_alerts: u32
branch_count: u32
revision_count: u32
thought_progress: f64            // thought_number / total_thoughts
budget_ratio: f64                // thought_number / budget.max
```

### Predict
- Called every thought
- Returns 0-1 trajectory score (probability this trace leads to good outcome)
- Microsecond inference, in-process

### Drift
- Called every thought
- Flags data drift (types of traces changing) and concept drift (strategies failing)
- Built into PerpetualBooster

### Train
- Called on trace completion + when late signals arrive (AR score)
- Incremental O(n) update
- Model file saved to disk periodically as backup
- No hardcoded outcome formula -- raw signals as features, PerpetualBooster learns what matters

### ML Signals (no formula, raw features to PerpetualBooster)

No hardcoded weights. All signals as raw feature columns. PerpetualBooster auto-learns importance.
Based on EvalAct process reward research: per-thought signals beat outcome-only signals.

**Process signals (per thought, immediate):**
1. **Warning responsiveness** -- did Claude address warnings from previous thought?
   Compare thought N warnings with thought N+1 content. Binary per warning: addressed or ignored.
2. **Confidence convergence** -- is the gap between reported and calculated shrinking?
   Converging = responding to feedback. Diverging = ignoring calibrator.
3. **Depth progression** -- are thoughts adding new info or rephrasing?
   From depth observer overlap scores across the trace.

**Outcome signals (per trace, on completion):**
4. **Trace Review trust score** -- single cheap external model via OpenRouter HTTP call from Rust binary.
   Runs immediately after every trace completes. One API call, ~1 second. Returns 0-10 trust score.
   This is the ML target variable. PerpetualBooster learns what process patterns produce trusted conclusions.
5. **Full AR** -- our own AR inspired by Valence's `/ar`. Reviews code/docs when task output is done.
   Separate tool, not wired to ML. Stands on its own as a review tool.
6. **Trace completion quality** -- clean (ADR generated, no unresolved contradictions,
   all warnings addressed) vs messy (abandoned, open contradictions, ignored warnings).

### Trace Review (locked in, tested)

Single HTTP POST from Rust binary to OpenRouter. No agent spawn, no tools, no Claude Code tokens.
Model: `openai/gpt-oss-20b:nitro` (cheap, fast).

**Prompt (locked in):**
```
System: You are a reasoning quality judge. You will be given a thinking
trace (a sequence of numbered thoughts). Answer one question: On a scale
of 0-10, how much would you trust the conclusion enough to act on it?
Respond with ONLY a JSON object: {"trust": <number>, "reason": "<one sentence>"}
```

**Tested results:**
| Trace type                        | Trust | Correct? |
|-----------------------------------|-------|----------|
| Simple bug fix, clean evidence    | 8     | Yes      |
| Overconfident, no evidence        | 3     | Yes      |
| Architecture with branching       | 7     | Yes      |
| Sycophantic, confirms first idea  | 2     | Yes      |
| Self-correcting with evidence     | 8     | Yes      |

Solves the Reddit commenter's concern: "OP assumes their awesome solution doesn't hallucinate evidence."
Analyzers catch process problems. Trace Review catches whether the output is trustworthy.

### Full AR

Our own AR skill inspired by Valence's `/ar`. Reviews actual code/docs produced after task completion.
Not wired to ML. Stands alone as a quality review tool.

### recordoutcome MCP Tool

```
// After fast AR (automatic, every trace)
recordoutcome({
  traceId: "abc",
  trustScore: 7,
  trustReason: "Reasoning is logical but omits failure modes"
})
```

Feature vector per trace (fed to PerpetualBooster):
```
// Process signals (aggregated from per-thought)
warning_responsiveness_ratio: f64,    // warnings addressed / total warnings
confidence_convergence: f64,          // trend of confidence gap across trace
depth_progression_ratio: f64,         // new info thoughts / total thoughts

// Outcome signal -- fast AR (per trace, immediate)
trust_score: f64,                     // 0-10 from external model

// Trace completion
trace_completed_clean: bool,          // ADR generated, no open contradictions
contradictions_resolved_ratio: f64,
warnings_addressed_final_ratio: f64,

// Context
thinking_mode: categorical,
component_count: u32,
thought_count: u32,
branch_count: u32,
revision_count: u32,
```

---

## DB: SQLite Persistence

Single file, no daemon, created on first run.

### Tables

```sql
CREATE TABLE thoughts (
    id INTEGER PRIMARY KEY,
    trace_id TEXT NOT NULL,
    thought_number INTEGER NOT NULL,
    thought TEXT NOT NULL,
    thinking_mode TEXT,
    affected_components TEXT,        -- JSON array
    confidence_reported REAL,
    confidence_calculated REAL,
    evidence TEXT,                    -- JSON array
    branch_id TEXT,
    warnings TEXT,                   -- JSON array
    analyzer_output TEXT,            -- JSON blob
    ml_prediction REAL,
    ml_drift INTEGER,
    recap TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE traces (
    trace_id TEXT PRIMARY KEY,
    thinking_mode TEXT,
    component_count INTEGER,
    thought_count INTEGER,
    branch_count INTEGER,
    trust_score REAL,                -- 0-10, from trace review (OpenRouter)
    trust_reason TEXT,               -- one-sentence explanation
    adr TEXT,                        -- generated ADR skeleton
    created_at TEXT NOT NULL,
    closed_at TEXT
);

CREATE TABLE patterns (
    id INTEGER PRIMARY KEY,
    thinking_mode TEXT,
    components TEXT,                  -- JSON array
    strategy TEXT,
    keywords TEXT,                    -- JSON array
    success_score REAL,              -- rolling average of auto_outcomes
    use_count INTEGER DEFAULT 1,
    last_used TEXT NOT NULL
);
```

### Write Pattern
- `db.write_thought()`: async after every thought, best-effort
- `db.flush_trace()`: on trace completion, writes trace summary + trust score from trace review
- `db.load_history()`: on startup, feeds ML bulk training
- All writes in catch -- failures log, never block

---

## Pruning

Runs on startup + configurable interval.

| Condition | Action |
|---|---|
| Trace without outcome, older than N days | Delete |
| Trace with <3 thoughts, no branches, no warnings | Delete after N/2 days |
| Traces with outcomes | Retain longer (configurable) |
| Patterns with use_count=1 older than N days | Delete |

---

## Response Format

Flat JSON, everything top-level. Claude sees all signals at a glance.

```json
{
    "traceId": "550e8400-e29b-41d4-a716-446655440000",
    "thoughtNumber": 3,
    "totalThoughts": 5,
    "nextThoughtNeeded": true,
    "branches": ["alt-sessions"],
    "thoughtHistoryLength": 3,
    "warnings": [
        "OVERCONFIDENCE: Reported 85%, calculated 40%. Back your claims with evidence.",
        "ANTI-QUICK-FIX: Shortcut language detected. Justify or propose proper solution."
    ],
    "confidenceReported": 85,
    "confidenceCalculated": 40,
    "confidenceGap": 45,
    "biasDetected": "anchoring",
    "sycophancy": null,
    "depthOverlap": 0.72,
    "budgetUsed": 3,
    "budgetMax": 5,
    "budgetCategory": "standard",
    "trajectory": 0.62,
    "driftDetected": false,
    "recap": "Thoughts 1-3: Identified auth service handles JWT + sessions. Proposed splitting. Flagged Redis shared dependency risk."
}
```

On completion (`nextThoughtNeeded: false`), adds:

```json
{
    "adr": "## ADR: Auth Service Split\n**Date**: 2026-04-06\n...",
    "trustScore": 7,
    "trustReason": "Reasoning is logical, explored alternatives, evidence cited"
}
```

---

## Config

TOML format. File: `config/feldspar.toml`. Validated at startup, panics on invalid. No fallback defaults.

### Hardcoded (no config, compile-time)
- MCP protocol version + JSON-RPC handling
- Analyzer pipeline structure (observers then evaluators)
- SQLite schema
- Warning regex patterns (MVP, make configurable later)

### Configurable (runtime, per project)

```toml
[feldspar]
db_path = "feldspar.db"          # SQLite file location
model_path = "feldspar.model"    # PerpetualBooster model file
recap_every = 3                  # recap frequency in thoughts

[trace_review]
api_key_env = "OPENROUTER_API_KEY"         # env var name, not the key itself
model = "openai/gpt-oss-20b:nitro"        # cheap fast model

[thresholds]
confidence_gap = 25              # |reported - calculated| > this fires OVERCONFIDENCE
over_analysis_multiplier = 1.5   # thoughtNumber > totalThoughts * this fires OVER-ANALYSIS
overthinking_multiplier = 2.0    # thoughtNumber > totalThoughts * this fires OVERTHINKING

[budgets]
minimal = [2, 3]                 # thought range for simple traces
standard = [3, 5]                # moderate complexity
deep = [5, 8]                    # architecture, scaling

[pruning]
no_outcome_days = 30             # traces without trust score, delete after
low_quality_days = 15            # <3 thoughts, no branches, no warnings
with_outcome_days = 90           # traces with trust scores, retain longer

[modes.architecture]             # custom thinking modes, extensible
requires = ["components"]        # required fields for this mode
budget = "deep"                  # which budget tier

[modes.performance]
requires = ["latency"]
budget = "standard"

[modes.debugging]
requires = ["evidence"]
budget = "standard"

[components]
valid = []                       # populate per project, prevents hallucination
```

### Warning Regex Patterns (hardcoded for MVP)

```rust
// Shortcut language
r"\b(just|simply)\s+(do|use|add|skip|ignore|throw|hack|slap)"
r"\bquick\s*(fix|solution|hack)"
r"\bgood\s+enough"
r"\bshould\s+be\s+fine"

// Dismissal language
r"\bpre.?existing\s+(issue|problem|bug)"
r"\bout\s+of\s+scope"
r"\bnot\s+(my|our)\s+(problem|concern)"
r"\b(already|was)\s+broken"
r"\bworked\s+before"
r"\bknown\s+issue"
```

---

## Server Architecture

Single daemon per project. MCP over HTTP on localhost. Multiple Claude Code sessions
connect to the same server. One ML model, one DB connection, always up to date.

```
feldspar daemon (one per project, long-running)
├── HTTP server on localhost:3581 (MCP over streamable HTTP)
├── accepts multiple concurrent connections (one per Claude Code session)
├── shared PerpetualBooster (single instance in memory, real-time learning)
├── shared SQLite (single connection, no concurrency issues)
├── tokio runtime for async: HTTP handling, DB writes, trace review, ML training
└── traces: HashMap<String, Trace> (concurrent traces across sessions)
```

### MCP Config (per project)

```json
{
  "mcpServers": {
    "feldspar": {
      "type": "http",
      "url": "http://localhost:3581"
    }
  }
}
```

### MCP Tools

One tool: `sequentialthinking`. Trace review is internal (HTTP call to OpenRouter from daemon).

### Async Model

Tokio runtime. HTTP requests handled async. Per-thought processing:

```
HTTP request arrives (MCP tools/call)
  → parse ThoughtInput
  → sync: append to trace, run analyzer pipeline, run ML predict (microseconds)
  → respond immediately to Claude
  → tokio::spawn: db.write_thought() (best-effort, non-blocking)
  → if trace complete:
      tokio::spawn: trace_review.review() (HTTP to OpenRouter, ~1 sec)
      tokio::spawn: ml.train() (incremental, milliseconds)
      tokio::spawn: db.flush_trace()
```

Response is never delayed by background work. Analyzer pipeline + ML predict are sync
and microsecond-fast. Everything else is fire-and-forget async.

### Error Handling

- Bad JSON → JSON-RPC error response
- Unknown method → error if has id, ignore if notification
- Tool error → isError: true in response
- Analyzer panic → catch_unwind, skip that analyzer, respond normally
- DB write failure → log, continue
- Trace review HTTP failure → log, no trust score for this trace
- ML train failure → log, model unchanged

---

## Startup Flow

```
1. Load config from config/feldspar.toml (panic on invalid)
2. Open/create SQLite file (WAL mode)
3. Run pruner
4. Load history from DB → bulk train PerpetualBooster
5. Load ML model file if exists (warm start)
6. Start HTTP server on localhost:3581
7. Ready
```

## Shutdown Flow

```
1. Stop accepting new connections
2. Flush all open traces to DB
3. Run trace review on any unclosed traces (best-effort)
4. Save ML model file to disk
5. Close DB
6. Exit
```
