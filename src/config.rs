use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

const VALID_REQUIRES: &[&str] = &["components", "evidence", "latency", "confidence"];

#[derive(Debug, Deserialize)]
pub struct Config {
    pub feldspar: FeldsparConfig,
    #[serde(alias = "trace_review")]
    pub llm: LlmConfig,
    pub thresholds: ThresholdsConfig,
    pub budgets: HashMap<String, [u32; 2]>,
    pub pruning: PruningConfig,
    pub modes: HashMap<String, ModeConfig>,
    pub components: ComponentsConfig,
    #[serde(skip)]
    pub principles: Vec<PrincipleGroup>,
}

#[derive(Debug, Deserialize)]
pub struct FeldsparConfig {
    pub db_path: String,
    pub model_path: String,
    pub recap_every: u32,
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub model: String,
}

#[derive(Debug, Deserialize)]
pub struct ThresholdsConfig {
    pub confidence_gap: f64,
    pub over_analysis_multiplier: f64,
    pub overthinking_multiplier: f64,
}

#[derive(Debug, Deserialize)]
pub struct PruningConfig {
    pub no_outcome_days: u32,
    pub low_quality_days: u32,
    pub with_outcome_days: u32,
}

#[derive(Debug, Deserialize)]
pub struct ModeConfig {
    pub requires: Vec<String>,
    pub budget: String,
    pub watches: String,
}

#[derive(Debug, Deserialize)]
pub struct ComponentsConfig {
    pub valid: Vec<String>,
}

// Stage 1: raw YAML parse target
#[derive(Debug, Deserialize)]
struct RawPrinciples {
    groups: HashMap<String, RawPrincipleGroup>,
}

#[derive(Debug, Deserialize)]
struct RawPrincipleGroup {
    #[serde(default)]
    active: bool,
    principles: Vec<Principle>,
}

// Stage 2: final types (map key injected as name)
#[derive(Debug, Clone)]
pub struct PrincipleGroup {
    pub name: String,
    pub active: bool,
    pub principles: Vec<Principle>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Principle {
    pub name: String,
    pub rule: String,
    #[serde(default)]
    pub ask: Vec<String>,
}

impl Config {
    pub fn load(toml_path: &str, principles_path: &str) -> Arc<Config> {
        let toml_str = std::fs::read_to_string(toml_path)
            .unwrap_or_else(|e| panic!("failed to read config '{}': {}", toml_path, e));
        let mut config: Config = toml::from_str(&toml_str)
            .unwrap_or_else(|e| panic!("failed to parse config '{}': {}", toml_path, e));

        let principles = load_principles(principles_path);
        validate(&config, &principles);
        config.principles = principles;
        Arc::new(config)
    }
}

fn load_principles(path: &str) -> Vec<PrincipleGroup> {
    let toml_str = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read principles file '{}': {}", path, e));

    let raw: RawPrinciples = toml::from_str(&toml_str)
        .unwrap_or_else(|e| panic!("failed to parse principles TOML '{}': {}", path, e));

    raw.groups
        .into_iter()
        .filter(|(_, group)| group.active)
        .map(|(name, group)| PrincipleGroup {
            name,
            active: group.active,
            principles: group.principles,
        })
        .collect()
}

fn validate(config: &Config, principles: &[PrincipleGroup]) {
    // Budget ranges: min <= max
    for (name, range) in &config.budgets {
        assert!(
            range[0] <= range[1],
            "budget '{}' has min > max: [{}, {}]",
            name,
            range[0],
            range[1]
        );
    }

    // Modes: budget tier exists, requires values are valid
    for (name, mode) in &config.modes {
        assert!(
            config.budgets.contains_key(&mode.budget),
            "mode '{}' references unknown budget tier '{}'",
            name,
            mode.budget
        );
        for req in &mode.requires {
            assert!(
                VALID_REQUIRES.contains(&req.as_str()),
                "mode '{}' requires unknown field '{}'. Valid: {}",
                name,
                req,
                VALID_REQUIRES.join(", ")
            );
        }
    }

    // Numeric sanity
    assert!(config.feldspar.recap_every >= 2, "recap_every must be >= 2 (LLM call per thought is too expensive)");
    assert!(
        config.pruning.no_outcome_days > 0,
        "pruning.no_outcome_days must be > 0"
    );
    assert!(
        config.pruning.low_quality_days > 0,
        "pruning.low_quality_days must be > 0"
    );
    assert!(
        config.pruning.with_outcome_days > 0,
        "pruning.with_outcome_days must be > 0"
    );
    assert!(
        config.thresholds.confidence_gap > 0.0,
        "thresholds.confidence_gap must be > 0"
    );
    assert!(
        config.thresholds.over_analysis_multiplier > 0.0,
        "thresholds.over_analysis_multiplier must be > 0"
    );
    assert!(
        config.thresholds.overthinking_multiplier > 0.0,
        "thresholds.overthinking_multiplier must be > 0"
    );

    // Principles: active groups must have at least one principle
    for group in principles {
        assert!(
            !group.principles.is_empty(),
            "principle group '{}' is active but has no principles",
            group.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            feldspar: FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
            },
            llm: LlmConfig {
                base_url: None,
                api_key_env: Some("TEST_KEY".into()),
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
            pruning: PruningConfig {
                no_outcome_days: 30,
                low_quality_days: 15,
                with_outcome_days: 90,
            },
            modes: HashMap::from([(
                "test-mode".into(),
                ModeConfig {
                    requires: vec![],
                    budget: "standard".into(),
                    watches: "test watches".into(),
                },
            )]),
            components: ComponentsConfig { valid: vec![] },
            principles: vec![],
        }
    }

    #[test]
    fn test_valid_config_parses() {
        let config = Config::load("config/feldspar.toml", "config/principles.toml");
        assert_eq!(config.feldspar.db_path, "feldspar.db");
        assert_eq!(config.feldspar.recap_every, 3);
        assert!(config.modes.contains_key("architecture"));
        assert!(config.budgets.contains_key("deep"));
        assert_eq!(config.thresholds.confidence_gap, 25.0);
    }

    #[test]
    fn test_principles_load() {
        let config = Config::load("config/feldspar.toml", "config/principles.toml");
        assert!(!config.principles.is_empty());
        let solid = config.principles.iter().find(|g| g.name == "solid");
        assert!(solid.is_some());
        assert!(!solid.unwrap().principles.is_empty());
    }

    #[test]
    fn test_principles_key_to_name() {
        let config = Config::load("config/feldspar.toml", "config/principles.toml");
        let names: Vec<&str> = config.principles.iter().map(|g| g.name.as_str()).collect();
        assert!(names.contains(&"solid"));
        assert!(names.contains(&"kiss-dry"));
        assert!(!names.contains(&"tdd"));
        assert!(!names.contains(&"security"));
    }

    #[test]
    fn test_inactive_groups_excluded() {
        let config = Config::load("config/feldspar.toml", "config/principles.toml");
        assert!(config.principles.iter().all(|g| g.name != "tdd"));
    }

    #[test]
    #[should_panic]
    fn test_invalid_toml_panics() {
        let _: Config = toml::from_str("not valid toml {{{{").unwrap();
    }

    #[test]
    #[should_panic(expected = "unknown budget tier")]
    fn test_unknown_budget_tier_panics() {
        let mut config = test_config();
        config.modes.insert(
            "bad-mode".into(),
            ModeConfig {
                requires: vec![],
                budget: "nonexistent".into(),
                watches: "x".into(),
            },
        );
        validate(&config, &[]);
    }

    #[test]
    #[should_panic(expected = "has min > max")]
    fn test_budget_min_gt_max_panics() {
        let mut config = test_config();
        config.budgets.insert("bad".into(), [5, 2]);
        validate(&config, &[]);
    }

    #[test]
    #[should_panic(expected = "recap_every must be >= 2")]
    fn test_recap_every_zero_panics() {
        let mut config = test_config();
        config.feldspar.recap_every = 0;
        validate(&config, &[]);
    }

    #[test]
    fn test_llm_config_parses() {
        let config = Config::load("config/feldspar.toml", "config/principles.toml");
        assert_eq!(config.llm.model, "openai/gpt-oss-20b:nitro");
    }

    fn minimal_toml(llm_section: &str) -> String {
        format!(
            r#"
[feldspar]
db_path = "test.db"
model_path = "test.model"
recap_every = 3

{llm_section}

[thresholds]
confidence_gap = 25.0
over_analysis_multiplier = 1.5
overthinking_multiplier = 2.0

[budgets]
standard = [3, 5]

[pruning]
no_outcome_days = 30
low_quality_days = 15
with_outcome_days = 90

[modes]

[components]
valid = []
"#
        )
    }

    #[test]
    fn test_llm_config_alias_trace_review() {
        let toml = minimal_toml(
            "[trace_review]\napi_key_env = \"TEST_KEY\"\nmodel = \"test-model\"",
        );
        let config: Config = toml::from_str(&toml).expect("should parse with trace_review alias");
        assert_eq!(config.llm.model, "test-model");
    }

    #[test]
    fn test_llm_config_optional_base_url() {
        let toml = minimal_toml("[llm]\napi_key_env = \"TEST_KEY\"\nmodel = \"test-model\"");
        let config: Config = toml::from_str(&toml).expect("should parse without base_url");
        assert!(config.llm.base_url.is_none());
    }

    #[test]
    fn test_llm_config_optional_api_key_env() {
        let toml = minimal_toml("[llm]\nmodel = \"test-model\"");
        let config: Config = toml::from_str(&toml).expect("should parse without api_key_env");
        assert!(config.llm.api_key_env.is_none());
    }

    #[test]
    #[should_panic(expected = "recap_every must be >= 2")]
    fn test_recap_every_one_panics() {
        let mut config = test_config();
        config.feldspar.recap_every = 1;
        validate(&config, &[]);
    }

    #[test]
    #[should_panic(expected = "active but has no principles")]
    fn test_empty_active_group_panics() {
        let config = test_config();
        let bad_group = PrincipleGroup {
            name: "empty".into(),
            active: true,
            principles: vec![],
        };
        validate(&config, &[bad_group]);
    }

    #[test]
    #[should_panic(expected = "requires unknown field")]
    fn test_unknown_requires_panics() {
        let mut config = test_config();
        config.modes.insert(
            "bad-mode".into(),
            ModeConfig {
                requires: vec!["nonexistent".into()],
                budget: "standard".into(),
                watches: "x".into(),
            },
        );
        validate(&config, &[]);
    }
}
