// Budget observer: tracks thought budget.
// Implements Observer trait. Returns Observation::Budget { used, max, category }.
// No alerts — warning engine owns underthinking/overthinking alerts.
// Falls back to (thought_number, 5, "standard") when resolve_budget returns None.

use super::Observation;
use crate::config::Config;
use crate::thought::ThoughtInput;

pub struct BudgetObserver;

impl super::Observer for BudgetObserver {
    fn observe(
        &self,
        input: &ThoughtInput,
        _records: &[ThoughtInput],
        config: &Config,
    ) -> Observation {
        let (used, max, category) = match config.resolve_budget(input.thinking_mode.as_deref()) {
            Some((_, max, tier)) => (input.thought_number, max, tier),
            None => (input.thought_number, 5, "standard".into()),
        };
        Observation::Budget { used, max, category }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzers::{test_input, Observation, Observer, Observations};

    fn observe_budget(mode: Option<&str>, thought_number: u32) -> (u32, u32, String) {
        let config = crate::analyzers::tests::test_config();
        let mut input = test_input("a thought");
        input.thinking_mode = mode.map(|m| m.into());
        input.thought_number = thought_number;
        let obs = BudgetObserver.observe(&input, &[], &config);
        match obs {
            Observation::Budget { used, max, category } => (used, max, category),
            _ => panic!("expected Budget observation"),
        }
    }

    #[test]
    fn test_budget_observation_architecture() {
        let (used, max, category) = observe_budget(Some("architecture"), 3);
        assert_eq!(used, 3);
        assert_eq!(max, 8);
        assert_eq!(category, "deep");
    }

    #[test]
    fn test_budget_observation_implementation() {
        let (used, max, category) = observe_budget(Some("implementation"), 2);
        assert_eq!(used, 2);
        assert_eq!(max, 3);
        assert_eq!(category, "minimal");
    }

    #[test]
    fn test_budget_unknown_mode() {
        let (used, max, category) = observe_budget(Some("nonexistent"), 4);
        assert_eq!(used, 4);
        assert_eq!(max, 5);
        assert_eq!(category, "standard");
    }

    #[test]
    fn test_budget_no_alerts() {
        let config = crate::analyzers::tests::test_config();
        let mut input = test_input("a thought");
        input.thinking_mode = Some("architecture".into());
        input.thought_number = 3;
        let obs = BudgetObserver.observe(&input, &[], &config);
        let mut observations = Observations::default();
        observations.merge(obs);
        let alerts = observations.drain_alerts();
        assert!(alerts.is_empty(), "budget observer should produce no alerts");
    }
}
