// Bias observer: checks every thought for 5 cognitive biases.
// Implements Observer trait. Returns Observation::Bias { detected: Option<String> }.
//
// Checks (first match wins):
//   "anchoring"    -- conclusion matches first hypothesis (similarity > 0.7), no alternatives explored.
//   "confirmation" -- all evidence supports conclusion, zero counter-arguments or risks cited.
//   "sunk_cost"    -- past 1.5x budget, same approach, language like "already"/"so far"/"invested".
//   "availability" -- same 3-4 keywords in 75%+ of thoughts on current branch. Tunnel vision.
//   "overconfidence" -- confidence > 80 AND progress < 50%. Timing-based, complements confidence evaluator.
//
// Also consumed by: confidence evaluator (uses bias_detected for bias avoidance score, 0-10 pts).
