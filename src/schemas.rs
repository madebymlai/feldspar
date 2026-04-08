use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct BriefArtifact {
    pub requirements: Vec<Requirement>,
}

#[derive(Debug, Deserialize)]
pub struct Requirement {
    pub name: String,
    pub description: String,
    pub user_story: String,
}

#[derive(Debug, Deserialize)]
pub struct DesignArtifact {
    pub modules: Vec<Module>,
}

#[derive(Debug, Deserialize)]
pub struct Module {
    pub name: String,
    pub purpose: String,
    pub leverages: Vec<String>,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct ExecutionPlanArtifact {
    pub groups: Vec<Group>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Deserialize)]
pub struct Group {
    pub tasks: Vec<u32>,
    pub depends_on: Vec<u32>,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct Task {
    pub number: u32,
    pub name: String,
    pub leverages: Vec<String>,
    pub description: String,
    #[serde(default)]
    pub pseudocode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DiagnosisArtifact {
    pub diagnosis: Diagnosis,
}

#[derive(Debug, Deserialize)]
pub struct Diagnosis {
    pub symptom: String,
    pub root_cause: String,
    pub evidence: Vec<String>,
    pub fix: String,
    pub files_changed: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidationReportArtifact {
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub extras: Vec<Extra>,
}

#[derive(Debug, Deserialize)]
pub struct Claim {
    pub number: u32,
    pub text: String,
    pub status: String,
    pub location: String,
}

#[derive(Debug, Deserialize)]
pub struct Extra {
    pub description: String,
    pub location: String,
}

pub fn validate(artifact_type: &str, content: &str) -> Result<(), String> {
    match artifact_type {
        "brief" => toml::from_str::<BriefArtifact>(content)
            .map(|_| ())
            .map_err(|e| format!("Brief schema validation failed: {}", e)),
        "design" => toml::from_str::<DesignArtifact>(content)
            .map(|_| ())
            .map_err(|e| format!("Design schema validation failed: {}", e)),
        "execution_plan" => toml::from_str::<ExecutionPlanArtifact>(content)
            .map(|_| ())
            .map_err(|e| format!("Execution plan schema validation failed: {}", e)),
        "diagnosis" => toml::from_str::<DiagnosisArtifact>(content)
            .map(|_| ())
            .map_err(|e| format!("Diagnosis schema validation failed: {}", e)),
        "validation_report" => toml::from_str::<ValidationReportArtifact>(content)
            .map(|_| ())
            .map_err(|e| format!("Validation report schema validation failed: {}", e)),
        "code" => Ok(()),
        _ => Err(format!("Unknown artifact type: {}", artifact_type)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_brief_valid() {
        let toml = r#"
[[requirements]]
name = "Auth"
description = "User authentication"
user_story = "As a user, I want to log in"
"#;
        assert!(validate("brief", toml).is_ok());
    }

    #[test]
    fn test_validate_brief_missing_field() {
        let toml = r#"
[[requirements]]
name = "Auth"
description = "User authentication"
"#;
        assert!(validate("brief", toml).is_err());
    }

    #[test]
    fn test_validate_design_valid() {
        let toml = r#"
[[modules]]
name = "auth"
purpose = "Handle authentication"
leverages = ["jwt"]
description = "JWT-based auth module"
"#;
        assert!(validate("design", toml).is_ok());
    }

    #[test]
    fn test_validate_execution_plan_valid() {
        let toml = r#"
[[groups]]
tasks = [1]
depends_on = []
description = "First group"

[[tasks]]
number = 1
name = "Setup"
leverages = []
description = "Initial setup"
"#;
        assert!(validate("execution_plan", toml).is_ok());
    }

    #[test]
    fn test_validate_diagnosis_valid() {
        let toml = r#"
[diagnosis]
symptom = "Crash on startup"
root_cause = "Null pointer in init"
evidence = ["logs/error.log:42"]
fix = "Add null check"
files_changed = ["src/init.rs"]
"#;
        assert!(validate("diagnosis", toml).is_ok());
    }

    #[test]
    fn test_validate_validation_report_valid() {
        let toml = r#"
[[claims]]
number = 1
text = "Module exists"
status = "pass"
location = "src/lib.rs"
"#;
        assert!(validate("validation_report", toml).is_ok());
    }

    #[test]
    fn test_validate_code_always_ok() {
        assert!(validate("code", "anything at all, not even valid TOML {{ }}").is_ok());
    }

    #[test]
    fn test_validate_unknown_type() {
        assert!(validate("bad", "").is_err());
    }
}
