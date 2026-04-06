// Config loader: reads TOML config, validates at startup, panics on invalid.
//
// Hardcoded (no config):
//   MCP protocol version, JSON-RPC handling, analyzer pipeline structure,
//   SQLite schema, warning regex patterns (MVP).
//
// Configurable (feldspar.toml):
//   [feldspar]        -- db_path, model_path, recap_every
//   [trace_review]    -- api_key_env, model (OpenRouter)
//   [thresholds]      -- confidence_gap, over_analysis_multiplier, overthinking_multiplier
//   [budgets]         -- minimal/standard/deep ranges
//   [pruning]         -- retention periods (no_outcome_days, low_quality_days, with_outcome_days)
//   [modes.*]         -- thinking modes with requires + budget tier (runtime-extensible)
//   [components]      -- valid component names (hallucination prevention)
//
// Entry point: Config::load() -> Config. Panics on invalid config. No fallback defaults.
