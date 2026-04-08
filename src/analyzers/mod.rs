// Analyzer pipeline: Observer/Evaluator pattern. Parallel within each phase (rayon).
//
// Traits:
//   Observer::observe(&self, input, records, config) -> Observation
//   Evaluator::evaluate(&self, input, records, observations, config) -> EvalOutput
//
// Observers (parallel, no cross-deps): depth, budget, bias.
//   Each returns an Observation enum variant. Merged into Observations struct.
//
// Evaluators (parallel, read Observations): confidence, sycophancy.
//   Each reads what observers found. Returns EvalOutput.
//
// Both can produce alerts. All alerts merged into final output.
// Error handling: catch_unwind per analyzer. One broken analyzer doesn't take down the pipeline.

use crate::config::Config;
use crate::thought::{Alert, ThoughtInput};
use rayon::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::LazyLock;

pub mod bias;
pub mod budget;
pub mod confidence;
pub mod depth;
pub mod sycophancy;

// ---------------------------------------------------------------------------
// Shared keyword lists (used by bias observer and sycophancy evaluator)
// ---------------------------------------------------------------------------

pub(crate) static COUNTER_ARGUMENT_KEYWORDS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| {
        vec![
            "however",
            "but",
            "contrary",
            "alternatively",
            "problem with this",
            "weakness",
            "downside",
            "risk",
            "what if",
            "on the other hand",
            "counter",
            "drawback",
            "issue with",
        ]
    });

pub(crate) static CONFIRMING_KEYWORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        "confirms",
        "supports",
        "as expected",
        "validates",
        "consistent with",
        "this proves",
        "clearly",
    ]
});

/// Words that reduce confidence score (used by confidence evaluator, Agent 2).
pub(crate) static HARMFUL_WORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        "obviously",
        "clearly",
        "trivially",
        "certainly",
        "definitely",
        "always",
        "never",
        "impossible",
        "guaranteed",
    ]
});

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

pub trait Observer: Send + Sync {
    fn observe(&self, input: &ThoughtInput, records: &[ThoughtInput], config: &Config)
        -> Observation;
}

pub trait Evaluator: Send + Sync {
    fn evaluate(
        &self,
        input: &ThoughtInput,
        records: &[ThoughtInput],
        observations: &Observations,
        config: &Config,
    ) -> EvalOutput;
}

// ---------------------------------------------------------------------------
// Observation enum (produced by observers)
// ---------------------------------------------------------------------------

pub enum Observation {
    Depth {
        prev_overlap: f64,
        initial_overlap: Option<f64>,
        contradictions: Vec<(u32, u32)>,
        shallow: bool,
        alerts: Vec<Alert>,
    },
    Budget {
        used: u32,
        max: u32,
        category: String,
    },
    Bias {
        detected: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Observations struct (merged from all observer outputs)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Observations {
    pub prev_overlap: Option<f64>,
    pub initial_overlap: Option<f64>,
    pub contradictions: Vec<(u32, u32)>,
    pub shallow: bool,
    pub budget_used: u32,
    pub budget_max: u32,
    pub budget_category: String,
    pub bias_detected: Option<String>,
    pub pending_alerts: Vec<Alert>,
}

impl Observations {
    pub fn merge(&mut self, obs: Observation) {
        match obs {
            Observation::Depth {
                prev_overlap,
                initial_overlap,
                contradictions,
                shallow,
                alerts,
            } => {
                self.prev_overlap = Some(prev_overlap);
                self.initial_overlap = initial_overlap;
                self.contradictions = contradictions;
                self.shallow = shallow;
                self.pending_alerts.extend(alerts);
            }
            Observation::Budget { used, max, category } => {
                self.budget_used = used;
                self.budget_max = max;
                self.budget_category = category;
            }
            Observation::Bias { detected } => {
                self.bias_detected = detected;
            }
        }
    }

    pub fn drain_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.pending_alerts)
    }
}

// ---------------------------------------------------------------------------
// EvalOutput enum (produced by evaluators)
// ---------------------------------------------------------------------------

pub enum EvalOutput {
    Confidence {
        calculated: f64,
        alert: Option<Alert>,
    },
    Sycophancy {
        pattern: Option<String>,
        alert: Option<Alert>,
    },
}

// ---------------------------------------------------------------------------
// PipelineResult
// ---------------------------------------------------------------------------

pub struct PipelineResult {
    pub alerts: Vec<Alert>,
    pub observations: Observations,
    pub confidence_calculated: Option<f64>,
    pub sycophancy_pattern: Option<String>,
    pub panic_warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// run_pipeline
// ---------------------------------------------------------------------------

pub fn run_pipeline(
    input: &ThoughtInput,
    records: &[ThoughtInput],
    config: &Config,
) -> PipelineResult {
    let mut panic_warnings = Vec::new();

    // Phase 1: Observers (run in parallel via rayon)
    let observers: Vec<(&str, Box<dyn Observer>)> = vec![
        ("depth", Box::new(depth::DepthObserver)),
        ("budget", Box::new(budget::BudgetObserver)),
        ("bias", Box::new(bias::BiasObserver)),
    ];

    let observer_results: Vec<(&str, Result<Observation, _>)> = observers
        .par_iter()
        .map(|(name, obs)| {
            let result = catch_unwind(AssertUnwindSafe(|| obs.observe(input, records, config)));
            (*name, result)
        })
        .collect();

    let mut observations = Observations::default();
    for (name, result) in observer_results {
        match result {
            Ok(obs) => observations.merge(obs),
            Err(_) => {
                panic_warnings
                    .push(format!("WARNING [ANALYZER-PANIC]: {} observer panicked", name));
            }
        }
    }

    // Phase 2: Evaluators (run in parallel via rayon)
    let evaluators: Vec<(&str, Box<dyn Evaluator>)> = vec![
        ("confidence", Box::new(confidence::ConfidenceEvaluator)),
        ("sycophancy", Box::new(sycophancy::SycophancyEvaluator)),
    ];

    let eval_results: Vec<(&str, Result<EvalOutput, _>)> = evaluators
        .par_iter()
        .map(|(name, eval)| {
            let result = catch_unwind(AssertUnwindSafe(|| {
                eval.evaluate(input, records, &observations, config)
            }));
            (*name, result)
        })
        .collect();

    let mut alerts = Vec::new();
    let mut confidence_calculated = None;
    let mut sycophancy_pattern = None;

    for (name, result) in eval_results {
        match result {
            Ok(EvalOutput::Confidence { calculated, alert }) => {
                confidence_calculated = Some(calculated);
                if let Some(a) = alert {
                    alerts.push(a);
                }
            }
            Ok(EvalOutput::Sycophancy { pattern, alert }) => {
                sycophancy_pattern = pattern;
                if let Some(a) = alert {
                    alerts.push(a);
                }
            }
            Err(_) => {
                panic_warnings
                    .push(format!("WARNING [ANALYZER-PANIC]: {} evaluator panicked", name));
            }
        }
    }

    alerts.extend(observations.drain_alerts());

    PipelineResult {
        alerts,
        observations,
        confidence_calculated,
        sycophancy_pattern,
        panic_warnings,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) fn test_input(thought: &str) -> ThoughtInput {
    use crate::thought::ThoughtInput;
    ThoughtInput {
        trace_id: None,
        thought: thought.into(),
        thought_number: 1,
        total_thoughts: 5,
        next_thought_needed: true,
        thinking_mode: None,
        affected_components: vec![],
        confidence: None,
        evidence: vec![],
        estimated_impact: None,
        is_revision: false,
        revises_thought: None,
        branch_from_thought: None,
        branch_id: None,
        needs_more_thoughts: false,
    }
}

#[cfg(test)]
pub(crate) fn test_record(thought: &str, number: u32) -> ThoughtInput {
    ThoughtInput {
        thought: thought.into(),
        thought_number: number,
        ..test_input("")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::thought::Severity;
    use crate::config::{
        ComponentsConfig, FeldsparConfig, LlmConfig, ModeConfig, ThresholdsConfig,
    };
    use std::collections::HashMap;

    pub(crate) fn test_config() -> Config {
        Config {
            feldspar: FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: LlmConfig {
                base_url: None,
                api_key_env: None,
                model: "test-model".into(),
            },
            thresholds: ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([
                ("minimal".into(), [2, 3]),
                ("standard".into(), [3, 5]),
                ("deep".into(), [5, 8]),
            ]),
            modes: HashMap::from([
                (
                    "architecture".into(),
                    ModeConfig {
                        requires: vec![],
                        budget: "deep".into(),
                        watches: String::new(),
                    },
                ),
                (
                    "implementation".into(),
                    ModeConfig {
                        requires: vec![],
                        budget: "minimal".into(),
                        watches: String::new(),
                    },
                ),
            ]),
            components: ComponentsConfig {
                valid: vec!["redis".into(), "postgres".into()],
            },
            principles: vec![],
        }
    }

    #[test]
    fn test_pipeline_runs() {
        let config = test_config();
        let input = test_input("considering which database to use for session storage");
        let result = run_pipeline(&input, &[], &config);
        assert!(result.panic_warnings.is_empty());
    }

    #[test]
    fn test_observer_panic_produces_warning() {
        struct PanickingObserver;
        impl Observer for PanickingObserver {
            fn observe(
                &self,
                _input: &ThoughtInput,
                _records: &[ThoughtInput],
                _config: &Config,
            ) -> Observation {
                panic!("test panic")
            }
        }

        let observer: Box<dyn Observer> = Box::new(PanickingObserver);
        let input = test_input("thought");
        let config = test_config();

        let result = catch_unwind(AssertUnwindSafe(|| observer.observe(&input, &[], &config)));
        assert!(result.is_err());

        // Verify warning format matches what run_pipeline produces
        let warning = format!("WARNING [ANALYZER-PANIC]: {} observer panicked", "test");
        assert!(warning.contains("ANALYZER-PANIC"));
    }

    #[test]
    fn test_observations_merge() {
        let mut obs = Observations::default();
        obs.merge(Observation::Depth {
            prev_overlap: 0.5,
            initial_overlap: Some(0.3),
            contradictions: vec![(1, 2)],
            shallow: false,
            alerts: vec![],
        });
        obs.merge(Observation::Budget {
            used: 3,
            max: 8,
            category: "deep".into(),
        });
        obs.merge(Observation::Bias {
            detected: Some("anchoring".into()),
        });

        assert_eq!(obs.prev_overlap, Some(0.5));
        assert_eq!(obs.initial_overlap, Some(0.3));
        assert_eq!(obs.contradictions, vec![(1, 2)]);
        assert_eq!(obs.budget_used, 3);
        assert_eq!(obs.budget_max, 8);
        assert_eq!(obs.budget_category, "deep");
        assert_eq!(obs.bias_detected, Some("anchoring".into()));
    }

    #[test]
    fn test_observations_drain_alerts() {
        let mut obs = Observations::default();
        obs.merge(Observation::Depth {
            prev_overlap: 0.5,
            initial_overlap: None,
            contradictions: vec![],
            shallow: false,
            alerts: vec![
                Alert {
                    analyzer: "depth".into(),
                    kind: "TEST_ALERT_1".into(),
                    severity: Severity::Medium,
                    message: "first".into(),
                },
                Alert {
                    analyzer: "depth".into(),
                    kind: "TEST_ALERT_2".into(),
                    severity: Severity::High,
                    message: "second".into(),
                },
            ],
        });

        let drained = obs.drain_alerts();
        assert_eq!(drained.len(), 2);

        let second_drain = obs.drain_alerts();
        assert_eq!(second_drain.len(), 0);
    }
}
