// Sycophancy evaluator: detects Claude agreeing with itself instead of analyzing.
// Implements Evaluator trait. Reads observations.initial_overlap for confirmation check.
//
// Three patterns (first match wins):
//
//   1. PREMATURE_AGREEMENT (Severity::Medium)
//      Trigger: thoughts 1-2 both agree with premise, no challenge language
//      ("however", "alternatively", "risk", "downside", "on the other hand").
//      Checked at thought_number == 2.
//
//   2. NO_SELF_CHALLENGE (Severity::Medium)
//      Trigger: 3+ consecutive thoughts on same branch with no branching and no revision.
//      Sliding window over last 3 thoughts.
//
//   3. CONFIRMATION_ONLY (Severity::High)
//      Trigger: next_thought_needed=false AND initial_overlap > 0.7 (thought 1 vs current)
//      AND zero revisions AND zero branches AND zero counter-argument keywords.
//      Most dangerous pattern -- appearance of depth without substance.

use crate::config::Config;
use crate::thought::{Alert, Severity, ThoughtInput};
use super::{EvalOutput, Observations, COUNTER_ARGUMENT_KEYWORDS};

pub struct SycophancyEvaluator;

impl super::Evaluator for SycophancyEvaluator {
    fn evaluate(
        &self,
        input: &ThoughtInput,
        records: &[ThoughtInput],
        observations: &Observations,
        _config: &Config,
    ) -> EvalOutput {
        // Pattern 1: Premature agreement (check at thought 2)
        if input.thought_number == 2 && !records.is_empty() {
            let t1 = records[0].thought.to_lowercase();
            let t2 = input.thought.to_lowercase();
            let t1_has_challenge = COUNTER_ARGUMENT_KEYWORDS.iter().any(|k| t1.contains(k));
            let t2_has_challenge = COUNTER_ARGUMENT_KEYWORDS.iter().any(|k| t2.contains(k));
            if !t1_has_challenge && !t2_has_challenge {
                return EvalOutput::Sycophancy {
                    pattern: Some("PREMATURE_AGREEMENT".into()),
                    alert: Some(Alert {
                        analyzer: "sycophancy".into(),
                        kind: "PREMATURE_AGREEMENT".into(),
                        severity: Severity::Medium,
                        message: "First 2 thoughts agree without challenging the premise. Consider counter-arguments.".into(),
                    }),
                };
            }
        }

        // Pattern 2: No self-challenge (3+ consecutive without branch/revision).
        // Skipped on the final thought — CONFIRMATION_ONLY takes priority there.
        if input.thought_number >= 3 && input.next_thought_needed {
            let branch_records: Vec<&ThoughtInput> = records
                .iter()
                .filter(|r| r.branch_id == input.branch_id)
                .collect();
            let any_challenge = branch_records
                .iter()
                .rev()
                .take(3)
                .any(|r| r.is_revision || r.branch_from_thought.is_some());
            if !any_challenge {
                return EvalOutput::Sycophancy {
                    pattern: Some("NO_SELF_CHALLENGE".into()),
                    alert: Some(Alert {
                        analyzer: "sycophancy".into(),
                        kind: "NO_SELF_CHALLENGE".into(),
                        severity: Severity::Medium,
                        message: "3+ thoughts without branching or revision. Challenge your own reasoning.".into(),
                    }),
                };
            }
        }

        // Pattern 3: Confirmation-only conclusion (most dangerous)
        if !input.next_thought_needed && records.len() >= 3 {
            let initial_sim = observations.initial_overlap.unwrap_or(0.0);
            if initial_sim > 0.7 {
                let any_revisions = records.iter().any(|r| r.is_revision);
                let any_branches = records.iter().any(|r| r.branch_from_thought.is_some());
                let has_counter_argument = records.iter().any(|r| {
                    let lower = r.thought.to_lowercase();
                    COUNTER_ARGUMENT_KEYWORDS.iter().any(|k| lower.contains(k))
                });

                if !any_revisions && !any_branches && !has_counter_argument {
                    return EvalOutput::Sycophancy {
                        pattern: Some("CONFIRMATION_ONLY".into()),
                        alert: Some(Alert {
                            analyzer: "sycophancy".into(),
                            kind: "CONFIRMATION_ONLY".into(),
                            severity: Severity::High,
                            message: format!(
                                "Final conclusion matches initial hypothesis ({:.0}% similar) with zero course corrections. This is confirmation bias, not analysis.",
                                initial_sim * 100.0
                            ),
                        }),
                    };
                }
            }
        }

        EvalOutput::Sycophancy { pattern: None, alert: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzers::{test_input, test_record, tests::test_config, Observations};
    use crate::analyzers::Evaluator;

    fn eval(
        input: &ThoughtInput,
        records: &[ThoughtInput],
        observations: &Observations,
    ) -> EvalOutput {
        let config = test_config();
        SycophancyEvaluator.evaluate(input, records, observations, &config)
    }

    fn pattern(output: EvalOutput) -> Option<String> {
        match output {
            EvalOutput::Sycophancy { pattern, .. } => pattern,
            _ => panic!("expected Sycophancy variant"),
        }
    }

    #[test]
    fn test_premature_agreement_fires() {
        let mut input = test_input("This looks great, I agree");
        input.thought_number = 2;
        let record = test_record("This approach is perfect", 1);
        let obs = Observations::default();
        assert_eq!(pattern(eval(&input, &[record], &obs)), Some("PREMATURE_AGREEMENT".into()));
    }

    #[test]
    fn test_premature_agreement_with_challenge_ok() {
        let mut input = test_input("This looks great");
        input.thought_number = 2;
        let record = test_record("However, this approach has risks", 1);
        let obs = Observations::default();
        assert_eq!(pattern(eval(&input, &[record], &obs)), None);
    }

    #[test]
    fn test_no_self_challenge_fires() {
        let mut input = test_input("continuing the same approach");
        input.thought_number = 4;
        let records = vec![
            test_record("thought 1", 1),
            test_record("thought 2", 2),
            test_record("thought 3", 3),
        ];
        let obs = Observations::default();
        assert_eq!(pattern(eval(&input, &records, &obs)), Some("NO_SELF_CHALLENGE".into()));
    }

    #[test]
    fn test_no_self_challenge_with_revision_ok() {
        let mut input = test_input("continuing the same approach");
        input.thought_number = 4;
        let mut records = vec![
            test_record("thought 1", 1),
            test_record("thought 2", 2),
            test_record("thought 3", 3),
        ];
        records[2].is_revision = true;
        let obs = Observations::default();
        assert_eq!(pattern(eval(&input, &records, &obs)), None);
    }

    #[test]
    fn test_confirmation_only_fires_initial_overlap() {
        // On the final thought (next_thought_needed=false), NO_SELF_CHALLENGE is skipped
        // so CONFIRMATION_ONLY can fire.
        let mut input = test_input("confirmed: same conclusion as initial");
        input.thought_number = 5;
        input.next_thought_needed = false;
        let records = vec![
            test_record("initial hypothesis", 1),
            test_record("middle thought", 2),
            test_record("more analysis", 3),
            test_record("still same direction", 4),
        ];
        let mut obs = Observations::default();
        obs.initial_overlap = Some(0.85);
        assert_eq!(pattern(eval(&input, &records, &obs)), Some("CONFIRMATION_ONLY".into()));
    }

    #[test]
    fn test_confirmation_only_with_counter_argument_ok() {
        let mut input = test_input("confirmed: same conclusion");
        input.thought_number = 5;
        input.next_thought_needed = false;
        let records = vec![
            test_record("initial hypothesis", 1),
            test_record("however, there are risks", 2),
            test_record("more analysis", 3),
            test_record("still same direction", 4),
        ];
        let mut obs = Observations::default();
        obs.initial_overlap = Some(0.85);
        assert_eq!(pattern(eval(&input, &records, &obs)), None);
    }

    #[test]
    fn test_confirmation_only_with_branch_ok() {
        let mut input = test_input("confirmed: same conclusion");
        input.thought_number = 5;
        input.next_thought_needed = false;
        let mut records = vec![
            test_record("initial hypothesis", 1),
            test_record("branch exploration", 2),
            test_record("more analysis", 3),
            test_record("still same direction", 4),
        ];
        records[1].branch_from_thought = Some(1);
        let mut obs = Observations::default();
        obs.initial_overlap = Some(0.85);
        assert_eq!(pattern(eval(&input, &records, &obs)), None);
    }

    #[test]
    fn test_confirmation_only_uses_initial_not_prev_overlap() {
        // initial_overlap is low (0.3) but prev_overlap is high (0.9).
        // Should NOT fire CONFIRMATION_ONLY because initial_overlap <= 0.7.
        let mut input = test_input("confirmed: same conclusion");
        input.thought_number = 5;
        input.next_thought_needed = false;
        let records = vec![
            test_record("initial hypothesis", 1),
            test_record("middle thought", 2),
            test_record("more analysis", 3),
            test_record("still same direction", 4),
        ];
        let mut obs = Observations::default();
        obs.initial_overlap = Some(0.3);
        obs.prev_overlap = Some(0.9);
        assert_eq!(pattern(eval(&input, &records, &obs)), None);
    }
}
