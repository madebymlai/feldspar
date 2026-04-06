// Depth observer: measures reasoning quality across the trace.
// Implements Observer trait. Returns Observation::Depth { overlap, contradictions }.
//
// Signals:
//   overlap: f64 (0-1) -- topic similarity between current thought and previous on same branch.
//     Uses strsim. Below 0.3 = PREMATURE_TOPIC_SWITCH alert.
//   contradictions: Vec<(u32, u32)> -- pairs of thought numbers that assert X and NOT-X without revision.
//     Scanned across full trace on same branch. Fires UNRESOLVED_CONTRADICTION alert.
//   Shallow check: if >50% of thoughts rephrase existing info, fires SHALLOW_ANALYSIS alert.
//
// Also consumed by: sycophancy evaluator (uses overlap for confirmation-only detection).
