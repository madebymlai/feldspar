// Bias observer: checks every thought for 5 cognitive biases. First match wins.
// Implements Observer trait. Returns Observation::Bias { detected: Option<String> }.
//
// Checks (in order):
//   "anchoring"      -- conclusion matches first hypothesis (strsim > 0.75), no branches.
//   "confirmation"   -- all confirming keywords present, zero counter-argument keywords.
//   "sunk_cost"      -- SET_A + SET_B two-keyword in thought, no question word.
//   "availability"   -- entity from config.components.valid in >40% of thoughts.
//   "overconfidence" -- confidence > 80 AND progress < 50%.

use super::Observation;
use crate::analyzers::{CONFIRMING_KEYWORDS, COUNTER_ARGUMENT_KEYWORDS};
use crate::config::Config;
use crate::thought::ThoughtInput;
use std::sync::LazyLock;
use strsim::normalized_levenshtein;

static SUNK_COST_ANCHORS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        "already built",
        "already implemented",
        "already written",
        "already invested",
        "we've spent",
        "can't throw away",
        "can't discard",
        "too much work",
        "not worth rewriting",
        "significant effort",
    ]
});

static SUNK_COST_CONTINUATIONS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        "so we should keep",
        "therefore we continue",
        "better to keep",
        "might as well",
        "instead of starting over",
        "instead of rewriting",
        "push through",
        "commit to",
        "worth continuing",
    ]
});

static QUESTION_WORDS: &[&str] = &["?", "should we", "why not", "what if", "or should", "could we"];

pub struct BiasObserver;

impl super::Observer for BiasObserver {
    fn observe(
        &self,
        input: &ThoughtInput,
        records: &[ThoughtInput],
        config: &Config,
    ) -> Observation {
        let detected = detect_anchoring(input, records)
            .or_else(|| detect_confirmation(input, records))
            .or_else(|| detect_sunk_cost(input))
            .or_else(|| detect_availability(input, records, config))
            .or_else(|| detect_overconfidence(input))
            .map(|s| s.to_string());

        Observation::Bias { detected }
    }
}

fn detect_anchoring(input: &ThoughtInput, records: &[ThoughtInput]) -> Option<&'static str> {
    let has_branch = records.iter().any(|r| r.branch_id != input.branch_id);
    if has_branch {
        return None;
    }

    let first = records.first()?;
    let sim = normalized_levenshtein(&input.thought, &first.thought);
    if sim > 0.75 {
        Some("anchoring")
    } else {
        None
    }
}

fn detect_confirmation(input: &ThoughtInput, records: &[ThoughtInput]) -> Option<&'static str> {
    let current = input.thought.to_lowercase();
    let all_thoughts: Vec<String> = records
        .iter()
        .map(|r| r.thought.to_lowercase())
        .chain(std::iter::once(current.clone()))
        .collect();

    let has_confirming = CONFIRMING_KEYWORDS
        .iter()
        .any(|kw| all_thoughts.iter().any(|t| t.contains(kw)));

    if !has_confirming {
        return None;
    }

    let has_counter = COUNTER_ARGUMENT_KEYWORDS
        .iter()
        .any(|kw| all_thoughts.iter().any(|t| t.contains(kw)));

    if has_counter {
        None
    } else {
        Some("confirmation")
    }
}

fn detect_sunk_cost(input: &ThoughtInput) -> Option<&'static str> {
    let thought = input.thought.to_lowercase();

    let has_anchor = SUNK_COST_ANCHORS.iter().any(|kw| thought.contains(kw));
    let has_continuation = SUNK_COST_CONTINUATIONS.iter().any(|kw| thought.contains(kw));

    if !(has_anchor && has_continuation) {
        return None;
    }

    let has_question = QUESTION_WORDS.iter().any(|kw| thought.contains(kw));
    if has_question {
        None
    } else {
        Some("sunk_cost")
    }
}

fn detect_availability(
    input: &ThoughtInput,
    records: &[ThoughtInput],
    config: &Config,
) -> Option<&'static str> {
    if config.components.valid.is_empty() {
        return None;
    }

    let all_thoughts: Vec<String> = records
        .iter()
        .map(|r| r.thought.to_lowercase())
        .chain(std::iter::once(input.thought.to_lowercase()))
        .collect();

    let total = all_thoughts.len();

    for component in &config.components.valid {
        let comp_lower = component.to_lowercase();
        let count = all_thoughts
            .iter()
            .filter(|t| t.contains(&comp_lower))
            .count();
        if count as f64 / total as f64 > 0.4 {
            return Some("availability");
        }
    }

    None
}

fn detect_overconfidence(input: &ThoughtInput) -> Option<&'static str> {
    let confidence = input.confidence?;
    let progress = input.thought_number as f64 / input.total_thoughts as f64;
    if confidence > 80.0 && progress < 0.5 {
        Some("overconfidence")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzers::{test_input, test_record, Observation, Observer};
    use crate::thought::ThoughtInput;

    fn observe(input: crate::thought::ThoughtInput, records: &[ThoughtInput]) -> Option<String> {
        let config = crate::analyzers::tests::test_config();
        let obs = BiasObserver.observe(&input, records, &config);
        match obs {
            Observation::Bias { detected } => detected,
            _ => panic!("expected Bias observation"),
        }
    }

    fn observe_with_config(
        input: crate::thought::ThoughtInput,
        records: &[ThoughtInput],
        config: crate::config::Config,
    ) -> Option<String> {
        let obs = BiasObserver.observe(&input, records, &config);
        match obs {
            Observation::Bias { detected } => detected,
            _ => panic!("expected Bias observation"),
        }
    }

    #[test]
    fn test_anchoring_detected() {
        // Strings are very similar (differ only by one word) → normalized_levenshtein > 0.75
        let rec = test_record("Redis is the best caching solution for session data storage", 1);
        let mut input = test_input("Redis is the right caching solution for session data storage");
        input.thought_number = 5;
        input.total_thoughts = 8;
        let result = observe(input, &[rec]);
        assert_eq!(result, Some("anchoring".into()));
    }

    #[test]
    fn test_confirmation_bias_detected() {
        // All thoughts contain confirming keywords, zero counter-arguments
        let rec1 = test_record("this confirms our earlier hypothesis about the design", 1);
        let rec2 = test_record("the results clearly support the initial approach", 2);
        let mut input = test_input("as expected, the data validates our implementation choice");
        input.thought_number = 3;
        let result = observe(input, &[rec1, rec2]);
        assert_eq!(result, Some("confirmation".into()));
    }

    #[test]
    fn test_sunk_cost_detected() {
        let mut input = test_input(
            "We already built the MongoDB schemas, so we should keep using it for consistency",
        );
        input.thought_number = 4;
        let result = observe(input, &[]);
        assert_eq!(result, Some("sunk_cost".into()));
    }

    #[test]
    fn test_sunk_cost_false_positive_question() {
        let mut input = test_input(
            "Already built MongoDB schemas, but should we switch to PostgreSQL?",
        );
        input.thought_number = 4;
        let result = observe(input, &[]);
        assert_ne!(result, Some("sunk_cost".into()));
    }

    #[test]
    fn test_availability_detected() {
        // "redis" appears in 3 of 5 total thoughts (60% > 40%)
        let rec1 = test_record("redis handles high throughput well for this use case", 1);
        let rec2 = test_record("we should configure redis with persistence enabled", 2);
        let rec3 = test_record("the latency requirements suggest a different approach", 3);
        let rec4 = test_record("redis cluster setup needs careful configuration", 4);
        let mut input = test_input("evaluating options for the caching layer implementation");
        input.thought_number = 5;
        let result = observe(input, &[rec1, rec2, rec3, rec4]);
        assert_eq!(result, Some("availability".into()));
    }

    #[test]
    fn test_availability_no_components_no_detection() {
        use crate::config::{
            ComponentsConfig, FeldsparConfig, LlmConfig, ThresholdsConfig,
        };
        use std::collections::HashMap;

        let config = crate::config::Config {
            feldspar: FeldsparConfig {
                db_path: "test.db".into(),
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
            budgets: HashMap::from([("standard".into(), [3u32, 5u32])]),
            modes: HashMap::new(),
            components: ComponentsConfig { valid: vec![] }, // empty — no detection
            ar: None,
            principles: vec![],
        };

        let rec1 = test_record("redis handles high throughput for this use case", 1);
        let rec2 = test_record("redis cluster configuration needs attention", 2);
        let mut input = test_input("redis is the primary caching solution here");
        input.thought_number = 3;
        let result = observe_with_config(input, &[rec1, rec2], config);
        assert_ne!(result, Some("availability".into()));
    }

    #[test]
    fn test_overconfidence_timing() {
        let mut input = test_input("this approach is definitely the best solution");
        input.thought_number = 2;
        input.total_thoughts = 8;
        input.confidence = Some(90.0);
        let result = observe(input, &[]);
        assert_eq!(result, Some("overconfidence".into()));
    }

    #[test]
    fn test_no_bias_clean_reasoning() {
        // Diverse thoughts with counter-arguments and branches — no bias
        let mut rec1 = test_record("PostgreSQL offers strong consistency guarantees", 1);
        rec1.branch_id = Some("branch-1".into());
        let rec2 = test_record("however, MongoDB might be simpler for this schema", 2);
        let mut input =
            test_input("on the other hand, we need to consider query complexity carefully");
        input.thought_number = 3;
        input.confidence = Some(50.0); // moderate confidence
        let result = observe(input, &[rec1, rec2]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_first_match_wins() {
        // Triggers both anchoring (high similarity to rec1) and
        // overconfidence (confidence=90, progress=25%). Anchoring is checked first.
        let rec = test_record("Redis is the best caching solution for session data storage", 1);
        let mut input = test_input("Redis is the right caching solution for session data storage");
        input.thought_number = 2;
        input.total_thoughts = 8;
        input.confidence = Some(90.0); // would trigger overconfidence too
        let result = observe(input, &[rec]);
        assert_eq!(result, Some("anchoring".into()));
    }
}
