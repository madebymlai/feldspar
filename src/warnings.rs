use crate::config::Config;
use crate::thought::{RecentProgress, ThoughtInput};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

struct WarningPattern {
    regex: Regex,
    label: &'static str,
    message: &'static str,
}

static LANGUAGE_PATTERNS: LazyLock<Vec<WarningPattern>> = LazyLock::new(|| {
    vec![
        // ANTI-QUICK-FIX patterns
        WarningPattern {
            regex: Regex::new(r"(?i)\b(just|simply)\s+(do|use|add|skip|ignore|throw|hack|slap)\b").unwrap(),
            label: "ANTI-QUICK-FIX",
            message: "Shortcut language detected — justify this approach or propose a proper solution.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bquick\s*(fix|solution|hack)\b").unwrap(),
            label: "ANTI-QUICK-FIX",
            message: "Shortcut language detected — justify this approach or propose a proper solution.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bgood\s+enough\b").unwrap(),
            label: "ANTI-QUICK-FIX",
            message: "Shortcut language detected — justify this approach or propose a proper solution.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bshould\s+be\s+fine\b").unwrap(),
            label: "ANTI-QUICK-FIX",
            message: "Shortcut language detected — justify this approach or propose a proper solution.",
        },
        // DISMISSAL patterns
        WarningPattern {
            regex: Regex::new(r"(?i)\bpre.?existing\s+(issue|problem|bug)").unwrap(),
            label: "DISMISSAL",
            message: "Dismissal language detected — address the issue or explain why it's out of scope.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bout\s+of\s+scope\b").unwrap(),
            label: "DISMISSAL",
            message: "Dismissal language detected — address the issue or explain why it's out of scope.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bnot\s+(my|our)\s+(problem|concern)\b").unwrap(),
            label: "DISMISSAL",
            message: "Dismissal language detected — address the issue or explain why it's out of scope.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\b(already|was)\s+broken\b").unwrap(),
            label: "DISMISSAL",
            message: "Dismissal language detected — address the issue or explain why it's out of scope.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bworked\s+before\b").unwrap(),
            label: "DISMISSAL",
            message: "Dismissal language detected — address the issue or explain why it's out of scope.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bknown\s+issue\b").unwrap(),
            label: "DISMISSAL",
            message: "Dismissal language detected — address the issue or explain why it's out of scope.",
        },
    ]
});

static BATCH_EXECUTION_PATTERNS: LazyLock<Vec<WarningPattern>> = LazyLock::new(|| {
    vec![
        WarningPattern {
            regex: Regex::new(r"(?i)\ball\s+tasks?\s+(at\s+once|together|simultaneously)\b").unwrap(),
            label: "BATCH_EXECUTION",
            message: "Batch execution detected — implement one task at a time, test each before moving on.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bimplement\s+tasks?\s+\d+\s*(through|to|-)\s*\d+\b").unwrap(),
            label: "BATCH_EXECUTION",
            message: "Batch execution detected — implement one task at a time, test each before moving on.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\blet\s+me\s+(do|implement|tackle|handle)\s+(everything|all\s+of)\b").unwrap(),
            label: "BATCH_EXECUTION",
            message: "Batch execution detected — implement one task at a time, test each before moving on.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\btackle\s+all\b.*\btogether\b").unwrap(),
            label: "BATCH_EXECUTION",
            message: "Batch execution detected — implement one task at a time, test each before moving on.",
        },
    ]
});

static TDD_BYPASS_PATTERNS: LazyLock<Vec<WarningPattern>> = LazyLock::new(|| {
    vec![
        WarningPattern {
            regex: Regex::new(r"(?i)\b(write|add|create)\s+tests?\s+later\b").unwrap(),
            label: "TDD_BYPASS",
            message: "TDD bypass detected — write tests before or alongside implementation. Red → Green → Refactor.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bskip\s+tests?\s+(for\s+now|first)\b").unwrap(),
            label: "TDD_BYPASS",
            message: "TDD bypass detected — write tests before or alongside implementation. Red → Green → Refactor.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\bimplement\s+first\b.*\btest\b").unwrap(),
            label: "TDD_BYPASS",
            message: "TDD bypass detected — write tests before or alongside implementation. Red → Green → Refactor.",
        },
        WarningPattern {
            regex: Regex::new(r"(?i)\btests?\s+can\s+wait\b").unwrap(),
            label: "TDD_BYPASS",
            message: "TDD bypass detected — write tests before or alongside implementation. Red → Green → Refactor.",
        },
    ]
});

fn check_language(thought: &str) -> Vec<String> {
    LANGUAGE_PATTERNS
        .iter()
        .filter(|p| p.regex.is_match(thought))
        .map(|p| format!("WARNING [{}]: {}", p.label, p.message))
        .collect()
}

fn check_budget(
    input: &ThoughtInput,
    recent_progress: &RecentProgress,
    config: &Config,
) -> Vec<String> {
    let mut warnings = Vec::new();

    let budget = match config.resolve_budget(input.thinking_mode.as_deref()) {
        Some(b) => b,
        None => {
            if let Some(ref mode) = input.thinking_mode {
                warnings.push(format!(
                    "WARNING [UNKNOWN-MODE]: thinking_mode '{}' not found in config. No budget checks applied.",
                    mode
                ));
            }
            return warnings;
        }
    };

    let (budget_min, _budget_max, _tier) = budget;
    let thought_num = input.thought_number as f64;
    let total = input.total_thoughts as f64;

    let over_analysis_threshold = total * config.thresholds.over_analysis_multiplier;
    let overthinking_threshold = total * config.thresholds.overthinking_multiplier;

    // OVERTHINKING (2.0x, suppresses OVER-ANALYSIS)
    if thought_num > overthinking_threshold {
        let has_progress = recent_progress
            .iter()
            .any(|(is_revision, branch_from)| *is_revision || branch_from.is_some());
        if !has_progress {
            warnings.push(format!(
                "WARNING [OVERTHINKING]: Past {}x your estimate with no new insights. Make a decision or branch.",
                config.thresholds.overthinking_multiplier
            ));
        }
    }
    // OVER-ANALYSIS (1.5x, only if not already overthinking)
    else if thought_num > over_analysis_threshold {
        warnings.push(format!(
            "WARNING [OVER-ANALYSIS]: At thought {} of estimated {}. Conclude or justify continuing.",
            input.thought_number, input.total_thoughts
        ));
    }

    // UNDERTHINKING
    if !input.next_thought_needed && input.thought_number < budget_min {
        warnings.push(format!(
            "WARNING [UNDERTHINKING]: Wrapping up in {} thoughts when minimum for this mode is {}. This needs more depth.",
            input.thought_number, budget_min
        ));
    }

    warnings
}

fn check_mode(input: &ThoughtInput, config: &Config) -> Vec<String> {
    let mut warnings = Vec::new();

    let mode_name = match input.thinking_mode.as_deref() {
        Some(m) => m,
        None => return warnings,
    };

    let mode_config = match config.modes.get(mode_name) {
        Some(m) => m,
        None => return warnings, // UNKNOWN-MODE already handled by check_budget
    };

    for req in &mode_config.requires {
        match req.as_str() {
            "evidence" if input.evidence.is_empty() => {
                warnings.push(format!(
                    "WARNING [NO-EVIDENCE]: {} mode requires citations — file paths, logs, stack traces.",
                    mode_name
                ));
            }
            "components" if input.affected_components.is_empty() => {
                warnings.push(format!(
                    "WARNING [NO-COMPONENTS]: {} mode requires naming affected components.",
                    mode_name
                ));
            }
            "latency" => {
                let missing = input.estimated_impact.as_ref()
                    .map_or(true, |imp| imp.latency.is_none());
                if missing {
                    warnings.push(format!(
                        "WARNING [NO-LATENCY]: {} mode requires latency estimates.",
                        mode_name
                    ));
                }
            }
            "confidence" if input.confidence.is_none() => {
                warnings.push(format!(
                    "WARNING [NO-CONFIDENCE]: {} mode requires a confidence rating.",
                    mode_name
                ));
            }
            _ => {}
        }
    }

    warnings
}

pub fn generate_warnings(
    input: &ThoughtInput,
    recent_progress: &RecentProgress,
    config: &Config,
) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(check_language(&input.thought));
    warnings.extend(check_budget(input, recent_progress, config));
    warnings.extend(check_mode(input, config));

    let mode = input.thinking_mode.as_deref();
    let is_impl_or_debug = matches!(mode, Some("implementation") | Some("debugging"));

    if is_impl_or_debug {
        for pattern in BATCH_EXECUTION_PATTERNS.iter() {
            if pattern.regex.is_match(&input.thought) {
                warnings.push(format!("WARNING [{}]: {}", pattern.label, pattern.message));
            }
        }
    }

    let tdd_active = config.principles.iter().any(|g| g.name == "tdd");
    if is_impl_or_debug && tdd_active {
        for pattern in TDD_BYPASS_PATTERNS.iter() {
            if pattern.regex.is_match(&input.thought) {
                warnings.push(format!("WARNING [{}]: {}", pattern.label, pattern.message));
            }
        }
    }

    // Dedup by label — keep first occurrence per [LABEL]
    let mut seen = HashSet::new();
    warnings.retain(|w| {
        let label = w.split(']').next().unwrap_or("");
        seen.insert(label.to_owned())
    });

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::thought::*;
    use std::collections::HashMap;

    fn test_config() -> Config {
        let mut modes = HashMap::new();
        modes.insert("architecture".into(), ModeConfig {
            requires: vec!["components".into()],
            budget: "deep".into(),
            watches: String::new(),
        });
        modes.insert("debugging".into(), ModeConfig {
            requires: vec!["evidence".into()],
            budget: "standard".into(),
            watches: String::new(),
        });
        modes.insert("implementation".into(), ModeConfig {
            requires: vec![],
            budget: "minimal".into(),
            watches: String::new(),
        });

        let mut budgets = HashMap::new();
        budgets.insert("minimal".into(), [2, 3]);
        budgets.insert("standard".into(), [3, 5]);
        budgets.insert("deep".into(), [5, 8]);

        Config {
            feldspar: FeldsparConfig {
                db_path: String::new(),
                model_path: String::new(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: LlmConfig {
                base_url: None,
                api_key_env: None,
                model: String::new(),
            },
            thresholds: ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets,
            modes,
            components: ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![],
        }
    }

    fn test_input(thought: &str) -> ThoughtInput {
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

    // --- Language tests ---

    #[test]
    fn test_anti_quick_fix_just_do() {
        let w = generate_warnings(&test_input("let's just do a quick hack"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("ANTI-QUICK-FIX")));
    }

    #[test]
    fn test_anti_quick_fix_should_be_fine() {
        let w = generate_warnings(&test_input("should be fine"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("ANTI-QUICK-FIX")));
    }

    #[test]
    fn test_dismissal_out_of_scope() {
        let w = generate_warnings(&test_input("that's out of scope"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("DISMISSAL")));
    }

    #[test]
    fn test_dismissal_known_issue() {
        let w = generate_warnings(&test_input("it's a known issue"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("DISMISSAL")));
    }

    #[test]
    fn test_clean_thought_no_warnings() {
        let w = generate_warnings(
            &test_input("Let's analyze the trade-offs between PostgreSQL and Redis"),
            &vec![],
            &test_config(),
        );
        assert!(w.is_empty());
    }

    #[test]
    fn test_case_insensitive() {
        let w = generate_warnings(&test_input("JUST DO it"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("ANTI-QUICK-FIX")));
    }

    #[test]
    fn test_dedup_same_label() {
        // "just do" and "quick hack" both match ANTI-QUICK-FIX — only one warning
        let w = generate_warnings(&test_input("let's just do a quick hack"), &vec![], &test_config());
        let count = w.iter().filter(|s| s.contains("ANTI-QUICK-FIX")).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_false_positive_acknowledged() {
        // Advisory: "simply use" fires ANTI-QUICK-FIX even for benign usage
        let w = generate_warnings(
            &test_input("we can simply use the existing trait implementation"),
            &vec![],
            &test_config(),
        );
        assert!(w.iter().any(|s| s.contains("ANTI-QUICK-FIX")));
    }

    // --- Budget tests ---

    #[test]
    fn test_over_analysis_fires() {
        let mut input = test_input("analyzing");
        input.thought_number = 8;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("OVER-ANALYSIS")));
    }

    #[test]
    fn test_over_analysis_within_limit() {
        // 7 < 7.5 (5 * 1.5) — no OVER-ANALYSIS
        let mut input = test_input("analyzing");
        input.thought_number = 7;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(!w.iter().any(|s| s.contains("OVER-ANALYSIS")));
    }

    #[test]
    fn test_overthinking_fires() {
        let mut input = test_input("still thinking");
        input.thought_number = 11;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let progress = vec![(false, None), (false, None), (false, None)];
        let w = generate_warnings(&input, &progress, &test_config());
        assert!(w.iter().any(|s| s.contains("OVERTHINKING")));
    }

    #[test]
    fn test_overthinking_suppressed_by_revision() {
        let mut input = test_input("still thinking");
        input.thought_number = 11;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let progress = vec![(true, None), (false, None), (false, None)];
        let w = generate_warnings(&input, &progress, &test_config());
        assert!(!w.iter().any(|s| s.contains("OVERTHINKING")));
    }

    #[test]
    fn test_overthinking_suppressed_by_new_branch() {
        let mut input = test_input("still thinking");
        input.thought_number = 11;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let progress = vec![(false, Some(9)), (false, None), (false, None)];
        let w = generate_warnings(&input, &progress, &test_config());
        assert!(!w.iter().any(|s| s.contains("OVERTHINKING")));
    }

    #[test]
    fn test_over_analysis_suppressed_when_overthinking() {
        let mut input = test_input("still thinking");
        input.thought_number = 11;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let progress = vec![(false, None), (false, None), (false, None)];
        let w = generate_warnings(&input, &progress, &test_config());
        assert!(w.iter().any(|s| s.contains("OVERTHINKING")));
        assert!(!w.iter().any(|s| s.contains("OVER-ANALYSIS")));
    }

    #[test]
    fn test_over_analysis_fires_alone_at_threshold() {
        // 8 > 7.5 (OVER-ANALYSIS) but 8 < 10 (OVERTHINKING)
        let mut input = test_input("analyzing");
        input.thought_number = 8;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("OVER-ANALYSIS")));
        assert!(!w.iter().any(|s| s.contains("OVERTHINKING")));
    }

    #[test]
    fn test_underthinking_fires() {
        let mut input = test_input("done");
        input.thought_number = 1;
        input.next_thought_needed = false;
        input.thinking_mode = Some("architecture".into()); // min=5
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("UNDERTHINKING")));
    }

    #[test]
    fn test_underthinking_ok_when_above_min() {
        let mut input = test_input("done");
        input.thought_number = 6;
        input.next_thought_needed = false;
        input.thinking_mode = Some("architecture".into()); // min=5
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(!w.iter().any(|s| s.contains("UNDERTHINKING")));
    }

    #[test]
    fn test_unknown_mode_fires_warning() {
        let mut input = test_input("thinking");
        input.thinking_mode = Some("nonexistent_mode".into());
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("UNKNOWN-MODE")));
    }

    #[test]
    fn test_no_mode_no_budget_warnings() {
        let input = test_input("thinking");
        let w = generate_warnings(&input, &vec![], &test_config());
        let budget_warnings: Vec<_> = w.iter().filter(|s| {
            s.contains("OVER-ANALYSIS") || s.contains("OVERTHINKING") ||
            s.contains("UNDERTHINKING") || s.contains("UNKNOWN-MODE")
        }).collect();
        assert!(budget_warnings.is_empty());
    }

    #[test]
    fn test_budget_threshold_float_boundary() {
        let config = test_config();
        // 8.0 > 7.5 (5 * 1.5) → fires
        let mut input = test_input("thinking");
        input.thought_number = 8;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        let w = generate_warnings(&input, &vec![], &config);
        assert!(w.iter().any(|s| s.contains("OVER-ANALYSIS")));

        // 7.0 < 7.5 → does not fire
        let mut input2 = test_input("thinking");
        input2.thought_number = 7;
        input2.total_thoughts = 5;
        input2.thinking_mode = Some("debugging".into());
        let w2 = generate_warnings(&input2, &vec![], &config);
        assert!(!w2.iter().any(|s| s.contains("OVER-ANALYSIS")));
    }

    // --- Mode tests ---

    #[test]
    fn test_no_evidence_debugging() {
        let mut input = test_input("debugging");
        input.thinking_mode = Some("debugging".into());
        input.evidence = vec![];
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("NO-EVIDENCE")));
    }

    #[test]
    fn test_no_components_architecture() {
        let mut input = test_input("architecture");
        input.thinking_mode = Some("architecture".into());
        input.affected_components = vec![];
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("NO-COMPONENTS")));
    }

    #[test]
    fn test_no_warning_when_fields_present() {
        let mut input = test_input("debugging with evidence");
        input.thinking_mode = Some("debugging".into());
        input.evidence = vec!["src/auth.rs".into()];
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(!w.iter().any(|s| s.contains("NO-EVIDENCE")));
    }

    #[test]
    fn test_unknown_mode_no_mode_warnings() {
        let mut input = test_input("thinking");
        input.thinking_mode = Some("nonexistent".into());
        let w = generate_warnings(&input, &vec![], &test_config());
        // UNKNOWN-MODE comes from budget checker, not mode checker
        assert!(!w.iter().any(|s| s.contains("NO-EVIDENCE")));
        assert!(!w.iter().any(|s| s.contains("NO-COMPONENTS")));
    }

    #[test]
    fn test_no_latency_custom_mode() {
        let mut config = test_config();
        config.modes.insert("perf".into(), ModeConfig {
            requires: vec!["latency".into()],
            budget: "standard".into(),
            watches: String::new(),
        });
        let mut input = test_input("performance analysis");
        input.thinking_mode = Some("perf".into());
        input.estimated_impact = None;
        let w = generate_warnings(&input, &vec![], &config);
        assert!(w.iter().any(|s| s.contains("NO-LATENCY")));
    }

    #[test]
    fn test_no_confidence_custom_mode() {
        let mut config = test_config();
        config.modes.insert("review".into(), ModeConfig {
            requires: vec!["confidence".into()],
            budget: "standard".into(),
            watches: String::new(),
        });
        let mut input = test_input("reviewing code");
        input.thinking_mode = Some("review".into());
        input.confidence = None;
        let w = generate_warnings(&input, &vec![], &config);
        assert!(w.iter().any(|s| s.contains("NO-CONFIDENCE")));
    }

    // --- BATCH_EXECUTION tests ---

    fn impl_input(thought: &str) -> ThoughtInput {
        let mut input = test_input(thought);
        input.thinking_mode = Some("implementation".into());
        input
    }

    #[test]
    fn test_batch_all_tasks_at_once() {
        let w = generate_warnings(&impl_input("Let me implement all tasks at once"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("BATCH_EXECUTION")));
    }

    #[test]
    fn test_batch_tasks_1_through_5() {
        let w = generate_warnings(&impl_input("I'll implement tasks 1 through 5"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("BATCH_EXECUTION")));
    }

    #[test]
    fn test_batch_do_everything() {
        let w = generate_warnings(&impl_input("Let me do everything first"), &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("BATCH_EXECUTION")));
    }

    #[test]
    fn test_batch_not_in_brainstorming() {
        let mut input = test_input("Let me implement all tasks at once");
        input.thinking_mode = Some("brainstorming".into());
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(!w.iter().any(|s| s.contains("BATCH_EXECUTION")));
    }

    #[test]
    fn test_batch_normal_task_mention() {
        let w = generate_warnings(&impl_input("Starting task 3 now"), &vec![], &test_config());
        assert!(!w.iter().any(|s| s.contains("BATCH_EXECUTION")));
    }

    // --- TDD_BYPASS tests ---

    fn tdd_config() -> Config {
        let mut config = test_config();
        config.principles = vec![crate::config::PrincipleGroup {
            name: "tdd".into(),
            active: true,
            principles: vec![crate::config::Principle {
                name: "TDD".into(),
                rule: "Red Green Refactor".into(),
                ask: vec![],
            }],
        }];
        config
    }

    #[test]
    fn test_tdd_write_tests_later() {
        let w = generate_warnings(&impl_input("I'll write tests later"), &vec![], &tdd_config());
        assert!(w.iter().any(|s| s.contains("TDD_BYPASS")));
    }

    #[test]
    fn test_tdd_skip_tests() {
        let w = generate_warnings(&impl_input("Skip tests for now and ship"), &vec![], &tdd_config());
        assert!(w.iter().any(|s| s.contains("TDD_BYPASS")));
    }

    #[test]
    fn test_tdd_implement_first() {
        let w = generate_warnings(&impl_input("Let me implement first then add the test"), &vec![], &tdd_config());
        assert!(w.iter().any(|s| s.contains("TDD_BYPASS")));
    }

    #[test]
    fn test_tdd_not_when_inactive() {
        let w = generate_warnings(&impl_input("I'll write tests later"), &vec![], &test_config());
        assert!(!w.iter().any(|s| s.contains("TDD_BYPASS")));
    }

    #[test]
    fn test_tdd_not_in_brainstorming() {
        let mut input = test_input("I'll write tests later");
        input.thinking_mode = Some("brainstorming".into());
        let w = generate_warnings(&input, &vec![], &tdd_config());
        assert!(!w.iter().any(|s| s.contains("TDD_BYPASS")));
    }

    #[test]
    fn test_tdd_normal_test_mention() {
        let w = generate_warnings(&impl_input("Running the test suite now"), &vec![], &tdd_config());
        assert!(!w.iter().any(|s| s.contains("TDD_BYPASS")));
    }

    // --- Integration test ---

    #[test]
    fn test_generate_warnings_merges_all() {
        let mut input = test_input("let's just do a quick hack");
        input.thought_number = 8;
        input.total_thoughts = 5;
        input.thinking_mode = Some("debugging".into());
        input.evidence = vec![];
        let w = generate_warnings(&input, &vec![], &test_config());
        assert!(w.iter().any(|s| s.contains("ANTI-QUICK-FIX")), "expected ANTI-QUICK-FIX");
        assert!(w.iter().any(|s| s.contains("OVER-ANALYSIS")), "expected OVER-ANALYSIS");
        assert!(w.iter().any(|s| s.contains("NO-EVIDENCE")), "expected NO-EVIDENCE");
    }
}
