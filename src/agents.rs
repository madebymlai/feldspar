use crate::config::Config;
use rand::Rng;
use serde::Deserialize;
use std::collections::HashMap;

const AGENT_ARM: &str = include_str!("../agents/arm.toml");
const AGENT_SOLVE: &str = include_str!("../agents/solve.toml");
const AGENT_BREAKDOWN: &str = include_str!("../agents/breakdown.toml");
const AGENT_BUILD: &str = include_str!("../agents/build.toml");
const AGENT_BUGFEST: &str = include_str!("../agents/bugfest.toml");
const AGENT_AR: &str = include_str!("../agents/ar.toml");
const AGENT_PMATCH: &str = include_str!("../agents/pmatch.toml");
const AGENT_ORCHESTRATOR: &str = include_str!("../agents/orchestrator.toml");

const UNIVERSAL_WARNINGS: &[&str] = &[
    "ANTI-QUICK-FIX: Shortcut language detected — justify or propose proper solution",
    "OVERCONFIDENCE: Reported confidence exceeds evidence — cite more or lower confidence",
    "UNDERTHINKING: Wrapping up too early — keep reasoning",
    "OVERTHINKING: Past budget with no new insights — decide or branch",
    "NO_SELF_CHALLENGE: 3+ thoughts without branching — explore an alternative",
    "CONFIRMATION_ONLY: Conclusion matches first thought with zero corrections — genuinely revise",
    "PATTERN_RISK: ML found similar poor traces — adjust approach",
];

#[derive(Debug, Deserialize)]
struct RawAgentToml {
    agent: RawAgentSection,
    prompt: RawPromptSection,
    warnings: RawWarningsSection,
    shutdown: RawShutdownSection,
}

#[derive(Debug, Deserialize)]
struct RawAgentSection {
    name: String,
    artifact_type: String,
    interactive: String,
    team: bool,
    ar_gated: bool,
    thinking_mode: String,
    #[serde(default)]
    fetches: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawPromptSection {
    identity: String,
    instructions: String,
}

#[derive(Debug, Deserialize)]
struct RawWarningsSection {
    mode: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawShutdownSection {
    instruction: String,
}

pub struct AgentDef {
    pub name: String,
    pub artifact_type: String,
    pub interactive: String,
    pub team: bool,
    pub ar_gated: bool,
    pub thinking_mode: String,
    pub prompt: String,
    pub mode_warnings: Vec<String>,
    pub shutdown: String,
    pub fetches: Vec<String>,
}

fn parse_agent_def(raw: RawAgentToml) -> AgentDef {
    AgentDef {
        name: raw.agent.name.clone(),
        artifact_type: raw.agent.artifact_type,
        interactive: raw.agent.interactive,
        team: raw.agent.team,
        ar_gated: raw.agent.ar_gated,
        thinking_mode: raw.agent.thinking_mode,
        prompt: format!(
            "{}\n\n{}",
            raw.prompt.identity.trim(),
            raw.prompt.instructions.trim()
        ),
        mode_warnings: raw.warnings.mode,
        shutdown: raw.shutdown.instruction,
        fetches: raw.agent.fetches,
    }
}

fn load_embedded_agents() -> HashMap<String, AgentDef> {
    let sources = [
        AGENT_ARM,
        AGENT_SOLVE,
        AGENT_BREAKDOWN,
        AGENT_BUILD,
        AGENT_BUGFEST,
        AGENT_AR,
        AGENT_PMATCH,
        AGENT_ORCHESTRATOR,
    ];
    let mut agents = HashMap::new();
    for src in sources {
        let raw: RawAgentToml = toml::from_str(src)
            .unwrap_or_else(|e| panic!("failed to parse agent TOML: {}", e));
        let name = raw.agent.name.clone();
        agents.insert(name, parse_agent_def(raw));
    }
    agents
}

fn load_custom_agents(agents: &mut HashMap<String, AgentDef>, dir: &std::path::Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "toml").unwrap_or(false) {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<RawAgentToml>(&content) {
                    Ok(raw) => {
                        let def = parse_agent_def(raw);
                        agents.insert(def.name.clone(), def);
                    }
                    Err(e) => tracing::warn!("failed to parse agent {:?}: {}", path, e),
                },
                Err(e) => tracing::warn!("failed to read agent {:?}: {}", path, e),
            }
        }
    }
}

pub fn load_agents(project_name: &str) -> HashMap<String, AgentDef> {
    let mut agents = load_embedded_agents();

    let home = dirs::home_dir().unwrap_or_default();
    let base = home.join("feldspar/data");

    load_custom_agents(&mut agents, &base.join("config/agents"));
    load_custom_agents(&mut agents, &base.join(project_name).join("config/agents"));

    agents
}

pub fn generate_prefix() -> String {
    let mut rng = rand::thread_rng();
    (0..4)
        .map(|_| {
            let idx = rng.gen_range(0..36u8);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

pub fn temper(agent: &AgentDef, config: &Config, prefix: &str) -> String {
    let mut output = String::new();

    output.push_str(&format!("PREFIX: {}\n\n", prefix));

    output.push_str(&agent.prompt);
    output.push_str("\n\n");

    let active_principles: Vec<_> = config.principles.iter().filter(|g| g.active).collect();
    if !active_principles.is_empty() {
        output.push_str("## Active Principles\n\n");
        for group in &active_principles {
            for p in &group.principles {
                output.push_str(&format!("- **{}**: {} — {}\n", group.name, p.name, p.rule));
            }
        }
        output.push('\n');
    }

    output.push_str("## Warnings (respond to these if they appear)\n\n");
    for w in UNIVERSAL_WARNINGS {
        output.push_str(&format!("- {}\n", w));
    }
    output.push('\n');

    if !agent.mode_warnings.is_empty() {
        output.push_str("## Mode-Specific Rules\n\n");
        for w in &agent.mode_warnings {
            output.push_str(&format!("- {}\n", w));
        }
        output.push('\n');
    }

    output.push_str("## Shutdown Protocol\n\n");
    output.push_str(agent.shutdown.trim());

    if agent.ar_gated {
        output.push_str("\n\n## Artifact Protocol\n\n");
        output.push_str(&format!("Your prefix is: {}\n", prefix));
        output.push_str("When your work is complete:\n");
        output.push_str("1. Call `submit` with your artifact name and content\n");
        output.push_str("2. Call `judge` with the artifact name to get a quality verdict\n");
        output.push_str("3. If verdict is \"revise\", address the feedback and repeat steps 1-2\n");
        output.push_str("4. If verdict is \"approve\", signal done to the orchestrator\n");
    }

    if !agent.fetches.is_empty() {
        output.push_str("\n\n## Context (fetch these before starting)\n\n");
        for (i, artifact_type) in agent.fetches.iter().enumerate() {
            output.push_str(&format!(
                "{}. Call `fetch` with prefix \"{}\" and type \"{}\"\n",
                i + 1,
                prefix,
                artifact_type
            ));
        }
        output.push('\n');
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Principle, PrincipleGroup};

    #[test]
    fn test_all_agent_tomls_parse() {
        let agents = load_agents("test");
        assert_eq!(agents.len(), 8);
    }

    #[test]
    fn test_agent_names() {
        let agents = load_agents("test");
        let expected = ["arm", "solve", "breakdown", "build", "bugfest", "ar", "pmatch"];
        for name in &expected {
            assert!(agents.contains_key(*name), "missing agent: {}", name);
        }
    }

    #[test]
    fn test_agent_fields_populated() {
        let agents = load_agents("test");
        for (name, def) in &agents {
            assert!(!def.name.is_empty(), "{}: name is empty", name);
            assert!(!def.thinking_mode.is_empty(), "{}: thinking_mode is empty", name);
            assert!(!def.prompt.is_empty(), "{}: prompt is empty", name);
            assert!(!def.shutdown.is_empty(), "{}: shutdown is empty", name);
        }
    }

    fn test_config_with_principles() -> Config {
        use std::collections::HashMap;
        Config {
            feldspar: crate::config::FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: crate::config::LlmConfig {
                base_url: None,
                api_key_env: None,
                model: "test-model".into(),
            },
            thresholds: crate::config::ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([("standard".into(), [3, 5])]),
            modes: HashMap::new(),
            components: crate::config::ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![PrincipleGroup {
                name: "solid".into(),
                active: true,
                principles: vec![Principle {
                    name: "SRP".into(),
                    rule: "One module, one reason to change.".into(),
                    ask: vec![],
                }],
            }],
        }
    }

    fn test_config_empty_principles() -> Config {
        use std::collections::HashMap;
        Config {
            feldspar: crate::config::FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: crate::config::LlmConfig {
                base_url: None,
                api_key_env: None,
                model: "test-model".into(),
            },
            thresholds: crate::config::ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([("standard".into(), [3, 5])]),
            modes: HashMap::new(),
            components: crate::config::ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![],
        }
    }

    #[test]
    fn test_temper_includes_principles() {
        let agents = load_agents("test");
        let agent = agents.get("build").unwrap();
        let config = test_config_with_principles();
        let output = temper(agent, &config, "test");
        assert!(output.contains("## Active Principles"), "missing Active Principles section");
        assert!(output.contains("SRP"), "missing principle name");
        assert!(output.contains("One module, one reason to change"), "missing principle rule");
    }

    #[test]
    fn test_temper_includes_warnings() {
        let agents = load_agents("test");
        let agent = agents.get("build").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(output.contains("ANTI-QUICK-FIX"), "missing universal warning");
        assert!(output.contains("Must stay focused on task scope"), "missing mode warning");
    }

    #[test]
    fn test_temper_includes_shutdown() {
        let agents = load_agents("test");
        let agent = agents.get("build").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(output.contains("SendMessage tool"), "missing shutdown protocol");
    }

    #[test]
    fn test_temper_empty_principles() {
        let agents = load_agents("test");
        let agent = agents.get("build").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(!output.contains("## Active Principles"), "should not have Active Principles section");
    }

    #[test]
    fn test_generate_prefix_length() {
        assert_eq!(generate_prefix().len(), 4);
    }

    #[test]
    fn test_generate_prefix_alphanumeric() {
        let p = generate_prefix();
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric() && (c.is_ascii_digit() || c.is_ascii_lowercase())));
    }

    #[test]
    fn test_generate_prefix_unique() {
        let mut unique = true;
        for _ in 0..10 {
            if generate_prefix() != generate_prefix() {
                unique = true;
                break;
            }
            unique = false;
        }
        assert!(unique, "generate_prefix should produce different values");
    }

    #[test]
    fn test_load_agents_embedded_only() {
        // Nonexistent project → only 7 embedded agents returned
        let agents = load_agents("__nonexistent_project_for_test__");
        assert_eq!(agents.len(), 8, "expected exactly 8 embedded agents");
    }

    #[test]
    fn test_load_custom_agents_from_dir() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let agent_toml = r#"
[agent]
name = "custom-test"
artifact_type = "code"
interactive = "background"
team = true
ar_gated = false
thinking_mode = "custom"

[prompt]
identity = "Custom test agent."
instructions = "Do the thing."

[warnings]
mode = []

[shutdown]
instruction = "Send shutdown_response."
"#;
        std::fs::write(tmp.path().join("custom-test.toml"), agent_toml).unwrap();

        let mut agents = HashMap::new();
        load_custom_agents(&mut agents, tmp.path());

        assert_eq!(agents.len(), 1);
        assert!(agents.contains_key("custom-test"));
    }

    #[test]
    fn test_custom_agent_overrides_embedded() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // Write agent named "build" to override embedded
        let agent_toml = r#"
[agent]
name = "build"
artifact_type = "docs"
interactive = "foreground"
team = false
ar_gated = false
thinking_mode = "custom-build"

[prompt]
identity = "Custom build override."
instructions = "Custom instructions."

[warnings]
mode = ["CUSTOM_WARNING"]

[shutdown]
instruction = "Custom shutdown."
"#;
        std::fs::write(tmp.path().join("build.toml"), agent_toml).unwrap();

        let mut agents = load_embedded_agents();
        load_custom_agents(&mut agents, tmp.path());

        let build = agents.get("build").unwrap();
        assert_eq!(build.thinking_mode, "custom-build", "custom agent should override embedded");
        assert_eq!(build.artifact_type, "docs");
    }

    #[test]
    fn test_invalid_custom_toml_skipped() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bad.toml"), "not valid toml [[[[").unwrap();

        let mut agents = HashMap::new();
        // Should not panic
        load_custom_agents(&mut agents, tmp.path());
        assert_eq!(agents.len(), 0);
    }

    #[test]
    fn test_solve_fetches_brief() {
        let agents = load_agents("test");
        let solve = agents.get("solve").unwrap();
        assert_eq!(solve.fetches, vec!["brief"]);
    }

    #[test]
    fn test_arm_fetches_empty() {
        let agents = load_agents("test");
        let arm = agents.get("arm").unwrap();
        assert!(arm.fetches.is_empty());
    }

    #[test]
    fn test_breakdown_fetches_two() {
        let agents = load_agents("test");
        let breakdown = agents.get("breakdown").unwrap();
        assert_eq!(breakdown.fetches, vec!["brief", "design"]);
    }

    #[test]
    fn test_temper_includes_prefix() {
        let agents = load_agents("test");
        let agent = agents.get("build").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "ab12");
        assert!(output.starts_with("PREFIX: ab12"), "output must start with prefix");
    }

    #[test]
    fn test_temper_ar_gated_has_artifact_protocol() {
        let agents = load_agents("test");
        let agent = agents.get("build").unwrap();
        assert!(agent.ar_gated, "build agent must be ar_gated for this test");
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(output.contains("## Artifact Protocol"), "missing Artifact Protocol section");
        assert!(output.contains("submit"), "missing submit instruction");
        assert!(output.contains("judge"), "missing judge instruction");
    }

    #[test]
    fn test_temper_non_ar_gated_no_artifact_protocol() {
        let agents = load_agents("test");
        let agent = agents.get("arm").unwrap();
        assert!(!agent.ar_gated, "arm agent must not be ar_gated for this test");
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(!output.contains("## Artifact Protocol"), "arm agent must not have Artifact Protocol");
    }

    #[test]
    fn test_temper_solve_has_fetch_instructions() {
        let agents = load_agents("test");
        let agent = agents.get("solve").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(output.contains("fetch"), "missing fetch keyword");
        assert!(output.contains("brief"), "missing brief artifact type");
        assert!(output.contains("Context (fetch these before starting)"), "missing context section");
    }

    #[test]
    fn test_temper_arm_no_fetch_instructions() {
        let agents = load_agents("test");
        let agent = agents.get("arm").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(!output.contains("Context (fetch"), "arm should not have Context (fetch section");
    }

    #[test]
    fn test_temper_breakdown_has_two_fetches() {
        let agents = load_agents("test");
        let agent = agents.get("breakdown").unwrap();
        let config = test_config_empty_principles();
        let output = temper(agent, &config, "test");
        assert!(output.contains("\"brief\""), "missing brief fetch instruction");
        assert!(output.contains("\"design\""), "missing design fetch instruction");
    }
}
