// Thought processor: core of feldspar.
//
// Types (input/output separation):
//   ThoughtInput  -- Deserialize from Claude's MCP JSON. Raw input only.
//   ThoughtResult -- Serialize to response + DB. All computed outputs (warnings, alerts, ML, recap, ADR).
//   ThoughtRecord -- ThoughtInput + ThoughtResult + timestamp. Stored in trace.
//   Impact        -- {latency?, throughput?, risk?} all Option<String>, not typed numbers.
//   Alert         -- {analyzer, kind, severity, message}. Single analyzer finding.
//   Severity      -- enum Medium | High. Closed set, not configurable.
//
// Core structs:
//   Trace          -- id (UUID), Vec<ThoughtRecord>, created_at, closed.
//   ThinkingServer -- HashMap<String, Trace>, shared DB pool, shared ML model, pruner.
//
// ThinkingMode is Option<String> validated against config at runtime. Not an enum.
// Branching is flat with tags (branch_id: Option<String>), not tree structure.
//
// Per thought flow:
//   parse ThoughtInput → append to trace → analyzer pipeline → ML predict/drift → warnings → recap → build ThoughtResult ��� async DB write → return response
//
// On completion (next_thought_needed=false):
//   generate ADR → compute auto_outcome from analyzer outputs → ML train → DB flush → evict trace
//
// On shutdown: flush all open traces, save ML model file, close DB.
