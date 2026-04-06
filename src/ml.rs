// PerpetualBooster ML: real-time per-thought inference + autonomous learning.
//
// No hardcoded outcome formula. Raw signals as features, PerpetualBooster auto-learns importance.
// One global model, thinking_mode as feature column. Handles categoricals natively.
//
// Signals (all as feature columns):
//   Process (per thought):  warning responsiveness, confidence convergence, depth progression.
//   Outcome (per trace):    trust score from trace review.
//   Outcome (per task):     AR score + critical/recommended/noted counts (optional, may be null).
//
// predict(features) -> f64: every thought, microsecond inference, 0-1 trajectory score.
// drift(features) -> DriftReport: every thought, flags data/concept drift.
// train(features): on trace completion + when async AR scores arrive. Incremental O(n).
//
// Pattern recall via leaf matching (PerpetualBooster's predict_nodes):
//   On trace completion: store leaf node set (HashSet<usize>) in DB.
//   On thought 1: predict_nodes() on current features → compare via Jaccard similarity
//   against stored past traces. Higher overlap = more similar by learned decision boundaries.
//   Returns top-K similar traces for formatting into patternRecall response.
//   PATTERN_RISK warning fires only when similar traces had low trust scores.
//
// On startup: bulk train from DB history. Model file saved to disk as backup.
// Best-effort: all calls wrapped in catch, failures return None, never block.
