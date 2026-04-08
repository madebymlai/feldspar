use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_FELDSPAR_TOML: &str = include_str!("../config/feldspar.toml");
const DEFAULT_PRINCIPLES_TOML: &str = include_str!("../config/principles.toml");

const FS_SKILL_MD: &str = include_str!("../skills/fs/SKILL.md");
const FS_CONFIG_MD: &str = include_str!("../skills/fs/config.md");

pub fn detect_project_name(override_name: Option<&str>) -> String {
    if let Some(name) = override_name {
        return name.to_owned();
    }
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|p| {
            Path::new(p.trim())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "default".into())
        })
}

pub fn data_dir(project_name: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("feldspar/data")
        .join(project_name)
}

pub fn user_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("feldspar/data/config")
}

pub fn create_data_dirs(project_name: &str) -> Result<(), String> {
    let project_dir = data_dir(project_name);
    std::fs::create_dir_all(project_dir.join("config"))
        .map_err(|e| format!("failed to create config dir: {}", e))?;
    std::fs::create_dir_all(project_dir.join("artifacts"))
        .map_err(|e| format!("failed to create artifacts dir: {}", e))?;
    Ok(())
}

pub fn prompt_api_key() -> String {
    use std::io::{self, BufRead, Write};
    loop {
        print!("OpenRouter API key (required): ");
        io::stdout().flush().ok();
        let mut key = String::new();
        io::stdin().lock().read_line(&mut key).ok();
        let key = key.trim().to_owned();
        if !key.is_empty() {
            return key;
        }
        println!(
            "API key is required for AR quality gate. Get one at https://openrouter.ai"
        );
    }
}

pub fn run_init(project_name: &str, project_dir: &Path, api_key: &str) -> Result<(), String> {
    // Write default configs (skip if already present)
    write_default_configs(project_name)?;

    // Write consumer project files
    write_mcp_json(project_dir, project_name, api_key)?;
    write_hooks_settings(project_dir)?;
    write_skill_files(project_dir)?;

    // Tmux setup (best-effort, non-blocking)
    run_tmux_setup();

    println!(
        "\nfeldspar initialized for project '{}'.\n\
         Data directory: {}\n\n\
         Next steps:\n\
         1. Start a new Claude Code session\n\
         2. feldspar will activate automatically via .mcp.json",
        project_name,
        data_dir(project_name).display()
    );

    Ok(())
}

fn write_default_configs(project_name: &str) -> Result<(), String> {
    let config_dir = data_dir(project_name).join("config");

    let feldspar_toml = config_dir.join("feldspar.toml");
    if !feldspar_toml.exists() {
        std::fs::write(&feldspar_toml, DEFAULT_FELDSPAR_TOML)
            .map_err(|e| format!("failed to write feldspar.toml: {}", e))?;
    }

    let principles_toml = config_dir.join("principles.toml");
    if !principles_toml.exists() {
        std::fs::write(&principles_toml, DEFAULT_PRINCIPLES_TOML)
            .map_err(|e| format!("failed to write principles.toml: {}", e))?;
    }

    Ok(())
}

fn write_mcp_json(project_dir: &Path, project_name: &str, api_key: &str) -> Result<(), String> {
    use serde_json::{json, Value};

    let mcp_path = project_dir.join(".mcp.json");
    let mut mcp: Value = if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path).unwrap_or_else(|_| "{}".into());
        serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let servers = mcp
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    servers.as_object_mut().unwrap().insert(
        "feldspar".into(),
        json!({
            "command": "feldspar",
            "args": ["start", "--project", project_name],
            "env": {
                "OPENROUTER_API_KEY": api_key,
                "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
            }
        }),
    );

    std::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp).unwrap())
        .map_err(|e| format!("failed to write .mcp.json: {}", e))
}

fn write_hooks_settings(project_dir: &Path) -> Result<(), String> {
    use serde_json::{json, Value};

    let claude_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| format!("failed to create .claude dir: {}", e))?;

    let settings_path = claude_dir.join("settings.local.json");
    let mut settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".into());
        serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks_obj = hooks.as_object_mut().unwrap();

    if !hooks_obj.contains_key("SessionStart") {
        hooks_obj.insert(
            "SessionStart".into(),
            json!([{
                "hooks": [{
                    "type": "command",
                    "command": "feldspar hook session-start"
                }]
            }]),
        );
    }

    if !hooks_obj.contains_key("PostToolUse") {
        hooks_obj.insert(
            "PostToolUse".into(),
            json!([{
                "hooks": [{
                    "type": "command",
                    "command": "feldspar hook record-change"
                }]
            }]),
        );
    }

    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .map_err(|e| format!("failed to write settings.local.json: {}", e))
}

fn write_skill_files(project_dir: &Path) -> Result<(), String> {
    let skills_dir = project_dir.join(".claude/skills/fs");
    std::fs::create_dir_all(&skills_dir)
        .map_err(|e| format!("failed to create skills/fs dir: {}", e))?;

    std::fs::write(skills_dir.join("SKILL.md"), FS_SKILL_MD)
        .map_err(|e| format!("failed to write SKILL.md: {}", e))?;
    std::fs::write(skills_dir.join("config.md"), FS_CONFIG_MD)
        .map_err(|e| format!("failed to write config.md: {}", e))?;

    Ok(())
}

fn run_tmux_setup() {
    let script = std::path::Path::new("scripts/setup-tmux.sh");
    if script.exists() {
        let _ = std::process::Command::new("bash")
            .arg(script)
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_project_name_override() {
        assert_eq!(detect_project_name(Some("foo")), "foo");
    }

    #[test]
    fn test_detect_project_name_fallback() {
        let name = detect_project_name(None);
        assert!(!name.is_empty());
    }

    #[test]
    fn test_data_dir_structure() {
        let dir = data_dir("test");
        let s = dir.to_string_lossy();
        assert!(s.ends_with("feldspar/data/test"), "got: {}", s);
    }

    #[test]
    fn test_user_config_dir() {
        let dir = user_config_dir();
        let s = dir.to_string_lossy();
        assert!(s.ends_with("feldspar/data/config"), "got: {}", s);
    }

    #[test]
    fn test_write_mcp_json_new() {
        let tmp = TempDir::new().unwrap();
        write_mcp_json(tmp.path(), "my-proj", "test-key").unwrap();
        let content = std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["feldspar"].is_object());
        assert_eq!(v["mcpServers"]["feldspar"]["args"][2], "my-proj");
        assert_eq!(v["mcpServers"]["feldspar"]["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1");
    }

    #[test]
    fn test_write_mcp_json_merge() {
        let tmp = TempDir::new().unwrap();
        let existing = serde_json::json!({
            "mcpServers": {
                "other-server": { "command": "other", "args": [] }
            }
        });
        std::fs::write(
            tmp.path().join(".mcp.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        write_mcp_json(tmp.path(), "my-proj", "test-key").unwrap();

        let content = std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["other-server"].is_object(), "other server should be preserved");
        assert!(v["mcpServers"]["feldspar"].is_object(), "feldspar entry should be added");
    }

    #[test]
    fn test_write_hooks() {
        let tmp = TempDir::new().unwrap();
        write_hooks_settings(tmp.path()).unwrap();
        let content =
            std::fs::read_to_string(tmp.path().join(".claude/settings.local.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["hooks"]["SessionStart"].is_array());
        assert!(v["hooks"]["PostToolUse"].is_array());
    }
}
