use crate::llm::LlmClient;
use crate::thought::Trace;

pub struct TrustScore {
    pub trust: f64,
    pub reason: String,
}

const SYSTEM_PROMPT: &str = "You are a reasoning quality judge. You will be given a thinking \
    trace and its thinking mode. Scale your expectations to the complexity \
    -- broader modes demand deeper reasoning. On a scale of 0-10, how much \
    would you trust the conclusion enough to act on it? Respond with ONLY \
    a JSON object: {\"trust\": <number>, \"reason\": \"<one sentence>\"}";

/// Format trace with structured branch labeling.
/// Main-line thoughts first in order, then each branch grouped under a header.
/// Mode is passed in by the caller — no panic paths here.
pub(crate) fn format_trace(trace: &Trace, mode: &str) -> String {
    let mut out = format!("Mode: {}\n---", mode);

    // Main-line thoughts (branch_id is None)
    for t in &trace.thoughts {
        if t.input.branch_id.is_none() {
            out.push_str(&format!("\nThought {}: {}", t.input.thought_number, t.input.thought));
        }
    }

    // Collect branch IDs in order of first appearance
    let mut seen = Vec::new();
    for t in &trace.thoughts {
        if let Some(ref bid) = t.input.branch_id {
            if !seen.contains(bid) {
                seen.push(bid.clone());
            }
        }
    }

    // Branch sections
    for bid in &seen {
        out.push_str(&format!("\n\nBranch {}:", bid));
        for t in &trace.thoughts {
            if t.input.branch_id.as_deref() == Some(bid) {
                out.push_str(&format!(
                    "\n  Thought {}: {}",
                    t.input.thought_number, t.input.thought
                ));
            }
        }
    }

    out
}

/// Call the judge model and parse the trust score.
/// Returns None on any failure (HTTP, parse, missing fields).
/// Caller is responsible for validating thinking_mode and API key presence.
pub async fn review(llm: &LlmClient, trace: &Trace, mode: &str) -> Option<TrustScore> {
    let formatted = format_trace(trace, mode);
    let json = llm.chat_json(SYSTEM_PROMPT, &formatted, 150).await?;
    let raw_trust = json["trust"].as_f64()?;
    let reason = json["reason"].as_str()?.to_owned();
    let trust = raw_trust.clamp(0.0, 10.0);
    if (trust - raw_trust).abs() > f64::EPSILON {
        tracing::warn!("trust score clamped from {} to {}", raw_trust, trust);
    }
    Some(TrustScore { trust, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thought::{Trace, ThoughtRecord, ThoughtInput, ThoughtResult};

    fn thought(num: u32, text: &str, mode: &str, branch: Option<&str>) -> ThoughtRecord {
        ThoughtRecord {
            input: ThoughtInput {
                trace_id: Some("t1".into()),
                thought: text.into(),
                thought_number: num,
                total_thoughts: 5,
                next_thought_needed: true,
                thinking_mode: Some(mode.into()),
                affected_components: vec![],
                confidence: None,
                evidence: vec![],
                estimated_impact: None,
                is_revision: false,
                revises_thought: None,
                branch_from_thought: None,
                branch_id: branch.map(|s| s.into()),
                needs_more_thoughts: false,
            },
            result: ThoughtResult::default(),
            created_at: 0,
        }
    }

    #[test]
    fn test_format_trace_main_line_only() {
        let mut trace = Trace::new();
        trace.thoughts = vec![
            thought(1, "First thought", "debugging", None),
            thought(2, "Second thought", "debugging", None),
        ];
        let out = format_trace(&trace, "debugging");
        assert!(out.starts_with("Mode: debugging\n---"));
        assert!(out.contains("Thought 1: First thought"));
        assert!(out.contains("Thought 2: Second thought"));
        assert!(!out.contains("Branch"));
    }

    #[test]
    fn test_format_trace_with_branches() {
        let mut trace = Trace::new();
        trace.thoughts = vec![
            thought(1, "Main start", "architecture", None),
            thought(2, "Branch exploration", "architecture", Some("alt-1")),
            thought(3, "Main continue", "architecture", None),
        ];
        let out = format_trace(&trace, "architecture");
        assert!(out.contains("Thought 1: Main start"));
        assert!(out.contains("Thought 3: Main continue"));
        assert!(out.contains("Branch alt-1:"));
        assert!(out.contains("  Thought 2: Branch exploration"));
        // Main-line thoughts should appear before branch sections
        let main_pos = out.find("Thought 1:").unwrap();
        let branch_pos = out.find("Branch alt-1:").unwrap();
        assert!(main_pos < branch_pos);
    }

    #[test]
    fn test_format_trace_branch_order_is_first_appearance() {
        let mut trace = Trace::new();
        trace.thoughts = vec![
            thought(1, "main", "debugging", None),
            thought(2, "z-branch thought", "debugging", Some("z-branch")),
            thought(3, "a-branch thought", "debugging", Some("a-branch")),
        ];
        let out = format_trace(&trace, "debugging");
        let z_pos = out.find("Branch z-branch:").unwrap();
        let a_pos = out.find("Branch a-branch:").unwrap();
        assert!(z_pos < a_pos, "z-branch should appear before a-branch (insertion order)");
    }

    #[test]
    fn test_format_trace_empty_trace() {
        let trace = Trace::new(); // no thoughts
        let out = format_trace(&trace, "debugging");
        assert_eq!(out, "Mode: debugging\n---");
    }
}
