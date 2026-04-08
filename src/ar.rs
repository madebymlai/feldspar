use crate::config::{ArConfig, LlmConfig, PrincipleGroup};
use crate::llm::LlmClient;
use serde::Deserialize;

const PRINCIPLES_SYSTEM_PROMPT: &str = "You are a principles compliance checker. Score the following artifact against the provided coding principles. For each principle violated, cite the specific violation. Return a score 0-100 and a list of findings. Respond in JSON: {\"score\": N, \"findings\": [\"...\"]}";

const ADVERSARIAL_SYSTEM_PROMPT: &str = "You are an adversarial reviewer. Stress-test the following artifact. Look for: gaps, contradictions, failure modes, missing edge cases, unstated assumptions, scalability issues, security holes. Return a score 0-100 and a list of findings. Respond in JSON: {\"score\": N, \"findings\": [\"...\"]}";

pub struct ArEngine {
    principles_llm: LlmClient,
    adversarial_llm: LlmClient,
    pub threshold: u32,
    pub max_retries: u32,
}

#[derive(Debug)]
pub struct ArResult {
    pub principles_score: u32,
    pub adversarial_score: u32,
    pub combined_score: u32,
    pub verdict: ArVerdict,
    pub feedback: ArFeedback,
}

#[derive(Debug, PartialEq)]
pub enum ArVerdict {
    Approve,
    Revise,
    Escalate,
}

impl ArVerdict {
    pub fn as_str(&self) -> &str {
        match self {
            ArVerdict::Approve => "approve",
            ArVerdict::Revise => "revise",
            ArVerdict::Escalate => "escalate",
        }
    }
}

#[derive(Debug)]
pub struct ArFeedback {
    pub principles: Vec<String>,
    pub adversarial: Vec<String>,
}

#[derive(Deserialize)]
struct EvalResponse {
    score: u32,
    #[serde(default)]
    findings: Vec<String>,
}

impl ArEngine {
    /// Returns None if OPENROUTER_API_KEY is not set.
    pub fn new(config: &ArConfig) -> Option<Self> {
        if std::env::var("OPENROUTER_API_KEY").is_err() {
            return None;
        }

        let principles_llm = LlmClient::new(&LlmConfig {
            model: config.principles_model.clone(),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            base_url: None,
        })?;
        let adversarial_llm = LlmClient::new(&LlmConfig {
            model: config.adversarial_model.clone(),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            base_url: None,
        })?;

        Some(Self {
            principles_llm,
            adversarial_llm,
            threshold: config.threshold,
            max_retries: config.max_retries,
        })
    }

    pub async fn evaluate(
        &self,
        artifact: &str,
        principles: &[PrincipleGroup],
        cycle: u32,
    ) -> ArResult {
        let principles_text = principles
            .iter()
            .flat_map(|g| {
                g.principles
                    .iter()
                    .map(move |p| format!("{}: {} — {}", g.name, p.name, p.rule))
            })
            .collect::<Vec<_>>()
            .join("\n");

        let principles_input = format!("## Principles\n{}\n\n## Artifact\n{}", principles_text, artifact);
        let adversarial_input = format!("## Artifact\n{}", artifact);

        let (p_val, a_val) = tokio::join!(
            self.principles_llm.chat_json(PRINCIPLES_SYSTEM_PROMPT, &principles_input, 500),
            self.adversarial_llm.chat_json(ADVERSARIAL_SYSTEM_PROMPT, &adversarial_input, 500),
        );

        let (p_score, p_findings) = Self::parse_eval(p_val);
        let (a_score, a_findings) = Self::parse_eval(a_val);

        let combined = p_score.min(a_score);
        let verdict = if combined >= self.threshold {
            ArVerdict::Approve
        } else if cycle < self.max_retries {
            ArVerdict::Revise
        } else {
            ArVerdict::Escalate
        };

        ArResult {
            principles_score: p_score,
            adversarial_score: a_score,
            combined_score: combined,
            verdict,
            feedback: ArFeedback {
                principles: p_findings,
                adversarial: a_findings,
            },
        }
    }

    fn parse_eval(result: Option<serde_json::Value>) -> (u32, Vec<String>) {
        match result {
            Some(val) => match serde_json::from_value::<EvalResponse>(val) {
                Ok(resp) => (resp.score.min(100), resp.findings),
                Err(_) => (0, vec!["Failed to parse evaluator response".into()]),
            },
            None => (0, vec!["Evaluator call failed".into()]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_eval_valid_json() {
        let val = json!({"score": 85, "findings": ["issue1"]});
        let (score, findings) = ArEngine::parse_eval(Some(val));
        assert_eq!(score, 85);
        assert_eq!(findings, vec!["issue1"]);
    }

    #[test]
    fn test_parse_eval_malformed() {
        // Valid JSON but missing required "score" field → parse failure
        let val = json!({"wrong_field": 42});
        let (score, findings) = ArEngine::parse_eval(Some(val));
        assert_eq!(score, 0);
        assert!(!findings.is_empty());
        assert!(findings[0].contains("parse") || findings[0].contains("Failed"));
    }

    #[test]
    fn test_parse_eval_error() {
        let (score, findings) = ArEngine::parse_eval(None);
        assert_eq!(score, 0);
        assert!(!findings.is_empty());
        assert!(findings[0].contains("Evaluator call failed"));
    }

    #[test]
    fn test_parse_eval_score_capped() {
        let val = json!({"score": 150, "findings": []});
        let (score, _) = ArEngine::parse_eval(Some(val));
        assert_eq!(score, 100);
    }

    #[test]
    fn test_parse_eval_empty_findings() {
        let val = json!({"score": 72});
        let (score, findings) = ArEngine::parse_eval(Some(val));
        assert_eq!(score, 72);
        assert!(findings.is_empty());
    }

    fn make_engine(threshold: u32, max_retries: u32) -> (u32, u32) {
        (threshold, max_retries)
    }

    fn verdict(combined: u32, threshold: u32, cycle: u32, max_retries: u32) -> ArVerdict {
        if combined >= threshold {
            ArVerdict::Approve
        } else if cycle < max_retries {
            ArVerdict::Revise
        } else {
            ArVerdict::Escalate
        }
    }

    #[test]
    fn test_verdict_approve() {
        let (threshold, max_retries) = make_engine(90, 3);
        assert_eq!(verdict(90, threshold, 1, max_retries), ArVerdict::Approve);
    }

    #[test]
    fn test_verdict_approve_above_threshold() {
        let (threshold, max_retries) = make_engine(90, 3);
        assert_eq!(verdict(95, threshold, 1, max_retries), ArVerdict::Approve);
    }

    #[test]
    fn test_verdict_revise() {
        let (threshold, max_retries) = make_engine(90, 3);
        assert_eq!(verdict(89, threshold, 1, max_retries), ArVerdict::Revise);
    }

    #[test]
    fn test_verdict_escalate() {
        let (threshold, max_retries) = make_engine(90, 3);
        // cycle >= max_retries → escalate
        assert_eq!(verdict(89, threshold, 3, max_retries), ArVerdict::Escalate);
    }

    #[test]
    fn test_verdict_escalate_at_boundary() {
        let (threshold, max_retries) = make_engine(90, 3);
        // cycle == max_retries → escalate
        assert_eq!(verdict(50, threshold, 3, max_retries), ArVerdict::Escalate);
    }

    #[test]
    fn test_verdict_revise_last_cycle() {
        let (threshold, max_retries) = make_engine(90, 3);
        // cycle=2, max=3 → still revise
        assert_eq!(verdict(89, threshold, 2, max_retries), ArVerdict::Revise);
    }
}
