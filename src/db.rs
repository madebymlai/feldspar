// SQLite persistence: single file, no daemon, created on first run. WAL mode.
//
// Tables:
//   thoughts   -- per-thought with all computed outputs as JSON
//   traces     -- summary + trust_score + trust_reason + AR scores (optional)
//   patterns   -- distilled strategies + rolling success score (for pattern recall)
//
// Core operations:
//   write_thought(): async best-effort after every thought, never blocks response.
//   flush_trace(): on completion, writes trace summary with trust score.
//   update_ar_scores(): when full AR completes, updates trace with AR findings.
//   load_history(): on startup, feeds PerpetualBooster bulk training.
//   find_similar(mode, components): pattern recall query for thought 1.
//     Returns past traces with same mode + overlapping components + their trust/AR scores.
//
// All writes in catch -- failures log, never block. Server runs fine without DB file.
