use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Deserialize, Serialize)]
pub struct BriefArtifact {
    pub requirements: Vec<Requirement>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Requirement {
    pub name: String,
    pub description: String,
    pub user_story: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DesignArtifact {
    pub modules: Vec<Module>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Module {
    pub name: String,
    pub purpose: String,
    pub leverages: Vec<String>,
    pub description: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecutionPlanArtifact {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Task {
    pub number: u32,
    pub name: String,
    pub group: String,
    pub depends_on: Vec<u32>,
    pub leverages: Vec<String>,
    pub description: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DiagnosisArtifact {
    pub diagnosis: Diagnosis,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Diagnosis {
    pub symptom: String,
    pub root_cause: String,
    pub evidence: Vec<String>,
    pub fix: String,
    pub files_changed: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ValidationReportArtifact {
    pub claims: Vec<Claim>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Claim {
    pub number: u32,
    pub text: String,
    pub status: String,
    pub location: String,
}

impl Requirement {
    pub fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Requirement identifier" },
                "description": { "type": "string", "description": "What is required" },
                "user_story": { "type": "string", "description": "As a [user], I want [goal] so that [reason]" }
            },
            "required": ["name", "description", "user_story"],
            "additionalProperties": false
        })
    }
}

impl Module {
    pub fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Module name" },
                "purpose": { "type": "string", "description": "What this module is responsible for" },
                "leverages": { "type": "array", "items": { "type": "string" }, "description": "Technologies/patterns used" },
                "description": { "type": "string", "description": "Detailed module description" }
            },
            "required": ["name", "purpose", "leverages", "description"],
            "additionalProperties": false
        })
    }
}

impl Task {
    pub fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "number": { "type": "integer", "description": "Task number" },
                "name": { "type": "string", "description": "Task name" },
                "group": { "type": "string", "pattern": "^[0-9]{2}$", "description": "Group identifier (NN format, e.g. 01, 02)" },
                "depends_on": { "type": "array", "items": { "type": "integer" }, "description": "Task numbers this depends on" },
                "leverages": { "type": "array", "items": { "type": "string" }, "description": "Technologies/patterns used" },
                "description": { "type": "string", "description": "What this task accomplishes" }
            },
            "required": ["number", "name", "group", "depends_on", "leverages", "description"],
            "additionalProperties": false
        })
    }
}

impl Diagnosis {
    pub fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "symptom": { "type": "string", "description": "Observable problem" },
                "root_cause": { "type": "string", "description": "Underlying cause" },
                "evidence": { "type": "array", "items": { "type": "string" }, "description": "Supporting evidence (file paths, logs)" },
                "fix": { "type": "string", "description": "How to fix the problem" },
                "files_changed": { "type": "array", "items": { "type": "string" }, "description": "Files that need to change" }
            },
            "required": ["symptom", "root_cause", "evidence", "fix", "files_changed"],
            "additionalProperties": false
        })
    }
}

impl Claim {
    pub fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "number": { "type": "integer", "description": "Claim number" },
                "text": { "type": "string", "description": "Claim text" },
                "status": { "type": "string", "enum": ["matched", "gap", "partial", "ambiguous"], "description": "Validation status" },
                "location": { "type": "string", "description": "Where in the codebase this claim applies" }
            },
            "required": ["number", "text", "status", "location"],
            "additionalProperties": false
        })
    }
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
[[tasks]]
number = 1
name = "Setup"
group = "01"
depends_on = []
leverages = []
description = "Initial setup"

[[tasks]]
number = 2
name = "Core logic"
group = "01"
depends_on = [1]
leverages = ["tokio"]
description = "Implement core"
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
status = "matched"
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
