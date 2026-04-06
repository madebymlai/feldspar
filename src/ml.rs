// PerpetualBooster ML: real-time per-thought inference + autonomous learning.
//
// No hardcoded outcome formula. Raw signals as features, PerpetualBooster auto-learns importance.
// Based on EvalAct process reward pattern: per-thought signals beat outcome-only signals.
//
// Signals (all as feature columns, no weights):
//   Process (per thought):  warning responsiveness, confidence convergence, depth progression.
//   Outcome (per trace):    AR review score (async), trace completion quality.
//   Cross-trace:            pattern success rate (self-calibrating prediction accuracy).
//
// predict(features) -> f64: every thought, microsecond inference, 0-1 trajectory score.
// drift(features) -> DriftReport: every thought, flags data/concept drift.
// train(features): on trace completion + when async signals arrive (AR score).
//   Incremental O(n). Model file saved to disk periodically as backup.
//
// On startup: bulk train from DB history.
// Best-effort: all calls wrapped in catch, failures return None, never block.
