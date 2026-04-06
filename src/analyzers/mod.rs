// Analyzer pipeline: Observer/Evaluator pattern. Parallel within each phase (rayon).
//
// Traits:
//   Observer::observe(&self, input, trace, config) -> Observation
//   Evaluator::evaluate(&self, input, trace, observations, config) -> Option<Alert>
//
// Observers (parallel, no cross-deps): depth, budget, bias.
//   Each returns an Observation enum variant. Merged into Observations struct.
//
// Evaluators (parallel, read Observations): confidence, sycophancy.
//   Each reads what observers found. Returns Option<Alert>.
//
// Both can produce alerts. All alerts merged into final output.
// Error handling: catch_unwind per analyzer. One broken analyzer doesn't take down the pipeline.
//
// Types:
//   Observation    -- enum { Depth{..}, Bias{..}, Budget{..} }
//   Observations   -- merged struct passed to evaluators
//   Alert          -- {analyzer, kind, severity, message}
//   Severity       -- enum Medium | High
//
// Entry point: run_pipeline(input, trace, config) -> (Vec<Alert>, Observations)
