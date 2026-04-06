// Confidence evaluator: independently scores confidence, flags overconfidence.
// Implements Evaluator trait. Reads observations.bias_detected for bias avoidance score.
//
// Scoring rubric (max 80 raw, normalized to 0-100):
//   Evidence cited:              0-30 pts (10 per citation, max 3)
//   Alternatives tried:          0-25 pts (any branch in trace = 25)
//   Unresolved contradictions:   -20 pts penalty (from observations.contradictions)
//   Depth/substantive ratio:     0-15 pts (filler pattern matching)
//   Bias avoidance:              0-10 pts (observations.bias_detected is None = 10)
//
// Alert:
//   OVERCONFIDENCE -- |reported - calculated| > 25 points. Severity::High.
//   Returns Alert with both reported and calculated values in message.
//
// Most impactful single analyzer. Claude is systematically overconfident.
