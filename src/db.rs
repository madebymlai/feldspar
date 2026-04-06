// SQLite persistence: single file, no daemon, created on first run.
// Tables: thoughts (per-thought with all computed outputs as JSON), traces (summary + auto_outcome + ADR), patterns (strategies + rolling success score).
// write_thought(): async best-effort after every thought, never blocks response.
// flush_trace(): on completion, writes trace summary with auto_outcome.
// load_history(): on startup, feeds PerpetualBooster bulk training from past traces with outcomes.
// All writes in catch -- failures log, never block. Server runs fine without DB file.
