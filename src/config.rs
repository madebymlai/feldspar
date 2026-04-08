use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const VALID_REQUIRES: &[&str] = &["components", "evidence", "latency", "confidence"];

#[derive(Debug, Deserialize)]
pub struct Config {
    pub feldspar: FeldsparConfig,
    #[serde(alias = "trace_review")]
    pub llm: LlmConfig,
    pub thresholds: ThresholdsConfig,
    pub budgets: HashMap<String, [u32; 2]>,
    pub modes: HashMap<String, ModeConfig>,
    pub components: ComponentsConfig,
    pub ar: Option<ArConfig>,
    #[serde(skip)]
    pub principles: Vec<PrincipleGroup>,
}

#[derive(Debug, Deserialize)]
pub struct ArConfig {
    pub threshold: u32,
    pub max_retries: u32,
    pub principles_model: String,
    pub adversarial_model: String,
}

#[derive(Debug, Deserialize)]
pub struct FeldsparConfig {
    pub db_path: String,
    pub model_path: String,
    pub recap_every: u32,
    #[serde(default = "default_top_k")]
    pub pattern_recall_top_k: u32,
    #[serde(default = "default_ml_budget")]
    pub ml_budget: f64,
    #[serde(default = "default_pattern_recall_min_traces")]
    pub pattern_recall_min_traces: u32,
}

fn default_top_k() -> u32 { 3 }
fn default_ml_budget() -> f64 { 0.5 }
fn default_pattern_recall_min_traces() -> u32 { 10 }

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
pub struct ModeConfig {
    pub requires: Vec<String>,
    pub budget: String,
    pub watches: String,
}

#[derive(Debug, Deserialize)]
pub struct ComponentsConfig {
    pub valid: Vec<String>,
}

// Stage 1: raw TOML parse target
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
    /// Resolve thinking mode to budget (min, max, tier_name).
    /// Returns None if mode is None or not found in config.
    pub fn resolve_budget(&self, mode: Option<&str>) -> Option<(u32, u32, String)> {
        let mode_name = mode?;
        let mode_config = self.modes.get(mode_name)?;
        let tier = &mode_config.budget;
        let range = self.budgets.get(tier)?;
        Some((range[0], range[1], tier.clone()))
    }

    /// Load config from 3-level merge: embedded defaults → user-level → project-level.
    pub fn load_merged(project_name: &str) -> Arc<Config> {
        // Level 1: embedded defaults
        let mut config: Config = toml::from_str(include_str!("../config/feldspar.toml"))
            .expect("embedded feldspar.toml must parse");
        let mut principles =
            load_principles_from_str(include_str!("../config/principles.toml"));

        // Level 2: user-level (optional)
        let user_dir = crate::init::user_config_dir();
        if let Some(user_config) = try_load_config(&user_dir.join("feldspar.toml")) {
            merge_config(&mut config, user_config);
        }
        if let Some(user_principles) = try_load_principles(&user_dir.join("principles.toml")) {
            merge_principles(&mut principles, user_principles);
        }

        // Level 3: project-level (optional)
        let project_dir = crate::init::data_dir(project_name).join("config");
        if let Some(proj_config) = try_load_config(&project_dir.join("feldspar.toml")) {
            merge_config(&mut config, proj_config);
        }
        if let Some(proj_principles) = try_load_principles(&project_dir.join("principles.toml")) {
            merge_principles(&mut principles, proj_principles);
        }

        config.principles = principles;
        validate(&config, &config.principles);
        Arc::new(config)
    }
}

fn try_load_config(path: &Path) -> Option<Config> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

fn try_load_principles(path: &Path) -> Option<Vec<PrincipleGroup>> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(load_principles_from_str(&content))
}

pub fn load_principles_from_str(content: &str) -> Vec<PrincipleGroup> {
    let raw: RawPrinciples = toml::from_str(content)
        .unwrap_or_else(|e| panic!("failed to parse principles: {}", e));
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

fn merge_config(base: &mut Config, overlay: Config) {
    for (name, mode) in overlay.modes {
        base.modes.insert(name, mode);
    }
    for (name, budget) in overlay.budgets {
        base.budgets.insert(name, budget);
    }
    base.thresholds = overlay.thresholds;
    if overlay.ar.is_some() {
        base.ar = overlay.ar;
    }
}

fn merge_principles(base: &mut Vec<PrincipleGroup>, overlay: Vec<PrincipleGroup>) {
    for og in overlay {
        if let Some(bg) = base.iter_mut().find(|g| g.name == og.name) {
            bg.active = og.active;
            bg.principles = og.principles;
        } else {
            base.push(og);
        }
    }
    base.retain(|g| g.active);
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

    // AR config: threshold 0-100, max_retries >= 1
    if let Some(ar) = &config.ar {
        assert!(ar.threshold <= 100, "ar.threshold must be <= 100");
        assert!(ar.max_retries >= 1, "ar.max_retries must be >= 1");
    }

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
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
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
            modes: HashMap::from([
                (
                    "test-mode".into(),
                    ModeConfig {
                        requires: vec![],
                        budget: "standard".into(),
                        watches: "test watches".into(),
                    },
                ),
                (
                    "architecture".into(),
                    ModeConfig {
                        requires: vec!["components".into()],
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
            components: ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![],
        }
    }

    #[test]
    fn test_valid_config_parses() {
        let config = Config::load_merged("nonexistent-test-project-xyz");
        assert_eq!(config.feldspar.db_path, "feldspar.db");
        assert_eq!(config.feldspar.recap_every, 3);
        assert!(config.modes.contains_key("architecture"));
        assert!(config.budgets.contains_key("deep"));
        assert_eq!(config.thresholds.confidence_gap, 25.0);
    }

    #[test]
    fn test_principles_load() {
        let config = Config::load_merged("nonexistent-test-project-xyz");
        assert!(!config.principles.is_empty());
        let solid = config.principles.iter().find(|g| g.name == "solid");
        assert!(solid.is_some());
        assert!(!solid.unwrap().principles.is_empty());
    }

    #[test]
    fn test_principles_key_to_name() {
        let config = Config::load_merged("nonexistent-test-project-xyz");
        let names: Vec<&str> = config.principles.iter().map(|g| g.name.as_str()).collect();
        assert!(names.contains(&"solid"));
        assert!(names.contains(&"kiss-dry"));
        assert!(names.contains(&"tdd"));
        assert!(!names.contains(&"security"));
    }

    #[test]
    fn test_inactive_groups_excluded() {
        let config = Config::load_merged("nonexistent-test-project-xyz");
        assert!(config.principles.iter().all(|g| g.name != "security"));
    }

    #[test]
    fn test_load_merged_embedded_defaults() {
        let config = Config::load_merged("nonexistent-project-zzz999");
        assert!(!config.modes.is_empty());
        assert!(!config.principles.is_empty());
        assert!(!config.budgets.is_empty());
    }

    #[test]
    fn test_merge_config_modes_override() {
        let mut base = test_config();
        let mut overlay = test_config();
        // Override architecture mode with different budget
        overlay.modes.insert(
            "architecture".into(),
            ModeConfig {
                requires: vec![],
                budget: "standard".into(),
                watches: "overridden".into(),
            },
        );
        merge_config(&mut base, overlay);
        assert_eq!(base.modes["architecture"].budget, "standard");
        assert_eq!(base.modes["architecture"].watches, "overridden");
    }

    #[test]
    fn test_merge_principles_deactivate() {
        let mut base = vec![PrincipleGroup {
            name: "tdd".into(),
            active: true,
            principles: vec![Principle {
                name: "TDD".into(),
                rule: "test first".into(),
                ask: vec![],
            }],
        }];
        // Overlay deactivates tdd
        let overlay = vec![PrincipleGroup {
            name: "tdd".into(),
            active: false,
            principles: vec![],
        }];
        merge_principles(&mut base, overlay);
        assert!(base.iter().all(|g| g.name != "tdd"), "tdd should be excluded");
    }

    #[test]
    fn test_merge_principles_add_group() {
        let mut base = vec![PrincipleGroup {
            name: "solid".into(),
            active: true,
            principles: vec![Principle {
                name: "SRP".into(),
                rule: "one reason".into(),
                ask: vec![],
            }],
        }];
        let overlay = vec![PrincipleGroup {
            name: "my-rules".into(),
            active: true,
            principles: vec![Principle {
                name: "custom".into(),
                rule: "my rule".into(),
                ask: vec![],
            }],
        }];
        merge_principles(&mut base, overlay);
        assert!(base.iter().any(|g| g.name == "my-rules"), "my-rules should be added");
    }

    #[test]
    fn test_load_principles_from_str() {
        let content = include_str!("../config/principles.toml");
        let groups = load_principles_from_str(content);
        assert!(!groups.is_empty());
        assert!(groups.iter().any(|g| g.name == "solid"));
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
        let config = Config::load_merged("nonexistent-test-project-xyz");
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

    #[test]
    fn test_resolve_budget_architecture() {
        let config = test_config();
        assert_eq!(config.resolve_budget(Some("architecture")), Some((5, 8, "deep".into())));
    }

    #[test]
    fn test_resolve_budget_implementation() {
        let config = test_config();
        assert_eq!(config.resolve_budget(Some("implementation")), Some((2, 3, "minimal".into())));
    }

    #[test]
    fn test_resolve_budget_unknown_mode() {
        let config = test_config();
        assert_eq!(config.resolve_budget(Some("nonexistent")), None);
    }

    #[test]
    fn test_resolve_budget_none_mode() {
        let config = test_config();
        assert_eq!(config.resolve_budget(None), None);
    }

    #[test]
    fn test_pattern_recall_top_k_default() {
        let toml = r#"
[feldspar]
db_path = "test.db"
model_path = "test.model"
recap_every = 3

[llm]
model = "test-model"

[thresholds]
confidence_gap = 25.0
over_analysis_multiplier = 1.5
overthinking_multiplier = 2.0

[budgets]
standard = [3, 5]

[modes]

[components]
valid = []
"#;
        let config: Config = toml::from_str(toml).expect("should parse");
        assert_eq!(config.feldspar.pattern_recall_top_k, 3);
    }

    #[test]
    fn test_pattern_recall_top_k_custom() {
        let toml = r#"
[feldspar]
db_path = "test.db"
model_path = "test.model"
recap_every = 3
pattern_recall_top_k = 5

[llm]
model = "test-model"

[thresholds]
confidence_gap = 25.0
over_analysis_multiplier = 1.5
overthinking_multiplier = 2.0

[budgets]
standard = [3, 5]

[modes]

[components]
valid = []
"#;
        let config: Config = toml::from_str(toml).expect("should parse");
        assert_eq!(config.feldspar.pattern_recall_top_k, 5);
    }

    fn minimal_feldspar_toml() -> &'static str {
        r#"
[feldspar]
db_path = "test.db"
model_path = "test.model"
recap_every = 3

[llm]
model = "test-model"

[thresholds]
confidence_gap = 25.0
over_analysis_multiplier = 1.5
overthinking_multiplier = 2.0

[budgets]
standard = [3, 5]

[modes]

[components]
valid = []
"#
    }

    #[test]
    fn test_ar_config_parses() {
        let toml = format!(
            "{}\n[ar]\nthreshold = 90\nmax_retries = 3\nprinciples_model = \"z-ai/glm-4.7-flash\"\nadversarial_model = \"openai/gpt-oss-120b:nitro\"\n",
            minimal_feldspar_toml()
        );
        let config: Config = toml::from_str(&toml).expect("should parse");
        assert!(config.ar.is_some());
        let ar = config.ar.unwrap();
        assert_eq!(ar.threshold, 90);
        assert_eq!(ar.max_retries, 3);
        assert_eq!(ar.principles_model, "z-ai/glm-4.7-flash");
        assert_eq!(ar.adversarial_model, "openai/gpt-oss-120b:nitro");
    }

    #[test]
    fn test_ar_config_optional() {
        let config: Config = toml::from_str(minimal_feldspar_toml()).expect("should parse");
        assert!(config.ar.is_none());
    }

    #[test]
    #[should_panic(expected = "ar.threshold must be <= 100")]
    fn test_ar_threshold_validation() {
        let mut config = test_config();
        config.ar = Some(ArConfig {
            threshold: 101,
            max_retries: 3,
            principles_model: "test".into(),
            adversarial_model: "test".into(),
        });
        validate(&config, &[]);
    }

    #[test]
    #[should_panic(expected = "ar.max_retries must be >= 1")]
    fn test_ar_max_retries_validation() {
        let mut config = test_config();
        config.ar = Some(ArConfig {
            threshold: 90,
            max_retries: 0,
            principles_model: "test".into(),
            adversarial_model: "test".into(),
        });
        validate(&config, &[]);
    }

    #[test]
    fn test_ml_budget_default() {
        let config: Config = toml::from_str(minimal_feldspar_toml()).expect("should parse");
        assert!((config.feldspar.ml_budget - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ml_budget_custom() {
        let toml = minimal_feldspar_toml().replace(
            "recap_every = 3",
            "recap_every = 3\nml_budget = 1.0",
        );
        let config: Config = toml::from_str(&toml).expect("should parse");
        assert!((config.feldspar.ml_budget - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_pattern_recall_min_traces_default() {
        let config: Config = toml::from_str(minimal_feldspar_toml()).expect("should parse");
        assert_eq!(config.feldspar.pattern_recall_min_traces, 10);
    }

    #[test]
    fn test_pattern_recall_min_traces_custom() {
        let toml = minimal_feldspar_toml().replace(
            "recap_every = 3",
            "recap_every = 3\npattern_recall_min_traces = 20",
        );
        let config: Config = toml::from_str(&toml).expect("should parse");
        assert_eq!(config.feldspar.pattern_recall_min_traces, 20);
    }
}
