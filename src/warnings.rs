// Warning engine: generates advisory alerts from thought content + trace state.
// All warnings fire. No cap, no dedup, no escalation. Simple like the Reddit post.
//
// Sources:
//   1. Warning engine checks (this module):
//      ANTI-QUICK-FIX: "quick fix", "hack", "just", "simply" in thought text.
//      OVER-ANALYSIS: thoughtNumber > totalThoughts * 1.5.
//      UNDERTHINKING: next_thought_needed=false with complex mode + many components.
//      OVERTHINKING: thoughtNumber > totalThoughts * 2, no new branches/revisions in last 3.
//      NO-LATENCY: performance mode without estimated_impact.latency.
//      NO-EVIDENCE: debugging mode without evidence citations.
//      NO-COMPONENTS: architecture mode without affected_components.
//      Mode-specific checks loaded from config (custom modes have custom requirements).
//   2. Analyzer alerts (from analyzers/mod.rs):
//      Converted to warning strings, appended to same array.
//
// All warnings advisory. Returned in flat JSON response. Claude can override with justification.
// Entry point: generate_warnings(input: &ThoughtInput, trace: &[ThoughtRecord], config: &Config) -> Vec<String>
