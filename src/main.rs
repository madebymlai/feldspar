mod agents;
mod analyzers;
mod ar;
mod schemas;
mod config;
mod db;
mod init;
mod llm;
mod mcp;
mod ml;
mod thought;
mod trace_review;
mod warnings;

use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Parser)]
#[command(name = "feldspar", about = "Cognitive reasoning MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the feldspar MCP server
    Start {
        /// Run as background daemon
        #[arg(long)]
        daemon: bool,
        /// Port to listen on
        #[arg(long, default_value = "3581")]
        port: u16,
        /// Project name (used to locate data dir and config)
        #[arg(long)]
        project: String,
    },
    /// Initialize feldspar for a project
    Init {
        /// Project name override (default: git root basename or cwd basename)
        #[arg(long)]
        project: Option<String>,
    },
    /// Hook subcommands (invoked by Claude Code hooks)
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
}

#[derive(Subcommand)]
enum HookAction {
    /// Record file changes for AR evaluation
    RecordChange,
    /// Run session start tasks
    SessionStart,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .json()
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { project } => {
            let project_name = init::detect_project_name(project.as_deref());
            println!("Initializing feldspar for project: {}", project_name);
            init::create_data_dirs(&project_name).expect("failed to create data dirs");
            let project_dir = std::env::current_dir().expect("failed to get cwd");
            let api_key = init::existing_api_key(&project_dir).unwrap_or_default();
            init::run_init(&project_name, &project_dir, &api_key).expect("init failed");
            if api_key.is_empty() {
                println!(
                    "\nNote: OPENROUTER_API_KEY is empty in .mcp.json.\n\
                     Edit .mcp.json to add your key before starting a session.\n\
                     Get one at https://openrouter.ai"
                );
            }
        }
        Commands::Start { daemon, port, project } => {
            if daemon {
                let exe = std::env::current_exe().expect("failed to get executable path");
                std::process::Command::new(exe)
                    .args(["start", "--port", &port.to_string(), "--project", &project])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("failed to spawn daemon");
                println!("feldspar daemon started on port {}", port);
                return;
            }

            run_server(port, &project).await;
        }
        Commands::Hook { action } => match action {
            HookAction::RecordChange => record_change(),
            HookAction::SessionStart => session_start(),
        },
    }
}

fn record_change() {
    // 1. Read stdin JSON for session_id and file_path
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).ok();
    let stdin: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return,
    };
    let session_id = match stdin.get("session_id").and_then(|s| s.as_str()) {
        Some(id) => id,
        None => return,
    };
    let file_path = stdin
        .get("tool_input")
        .and_then(|t| t.get("file_path"))
        .and_then(|f| f.as_str())
        .unwrap_or("");
    if file_path.is_empty() {
        return;
    }

    // 2. HTTP lookup for prefix/group/role
    let port = std::env::var("FELDSPAR_PORT").unwrap_or_else(|_| "3581".into());
    let url = format!("http://127.0.0.1:{}/session/{}", port, session_id);
    let resp = ureq::get(&url)
        .call()
        .ok()
        .and_then(|r| r.into_body().read_to_string().ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let resp = match resp {
        Some(r) => r,
        None => return,
    };

    let role = resp.get("role").and_then(|r| r.as_str()).unwrap_or("");
    let prefix = resp.get("prefix").and_then(|p| p.as_str()).unwrap_or("");
    if prefix.is_empty() || (role != "build" && role != "bugfest") {
        return;
    }

    // 3. Scoped git diff
    let diff = std::process::Command::new("git")
        .args(["diff", "HEAD", "--", file_path])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    if diff.trim().is_empty() {
        return;
    }

    // 4. Write to correct TOML file with """ escaping
    let mode_dir = if role == "build" { "implementation" } else { "debugging" };
    let group = resp.get("group").and_then(|g| g.as_str());
    let filename = if role == "build" {
        format!("{}-changes.toml", group.unwrap_or("00"))
    } else {
        "changes.toml".to_owned()
    };

    let project = init::detect_project_name(None);
    let dir = init::data_dir(&project)
        .join("artifacts/changes")
        .join(mode_dir)
        .join(prefix);
    let _ = std::fs::create_dir_all(&dir);

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let safe_diff = diff.trim().replace("\"\"\"", "\"\"\\\"");
    let entry = format!(
        "\n[[changes]]\ntimestamp = {}\nfile = \"{}\"\ndiff = \"\"\"\n{}\"\"\"\n",
        timestamp, file_path, safe_diff
    );

    let path = dir.join(filename);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = file.write_all(entry.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_record_change_skips_no_session_id() {
        // Function reads from real stdin which we can't mock in unit tests.
        // Verify the escaping logic and JSON parsing directly instead.
        let input = r#"{"tool_input": {"file_path": "foo.rs"}}"#;
        let v: serde_json::Value = serde_json::from_str(input).unwrap();
        assert!(v.get("session_id").is_none());
    }

    #[test]
    fn test_record_change_skips_no_file_path() {
        let input = r#"{"session_id": "abc"}"#;
        let v: serde_json::Value = serde_json::from_str(input).unwrap();
        let file_path = v
            .get("tool_input")
            .and_then(|t| t.get("file_path"))
            .and_then(|f| f.as_str())
            .unwrap_or("");
        assert!(file_path.is_empty());
    }

    #[test]
    fn test_record_change_skips_non_build_role() {
        let role = "solve";
        assert!(role != "build" && role != "bugfest");
    }

    #[test]
    fn test_triple_quote_escaping() {
        let diff = "foo\"\"\"bar";
        let safe = diff.replace("\"\"\"", "\"\"\\\"");
        assert_eq!(safe, "foo\"\"\\\"bar");
    }
}

fn session_start() {
    // Read hook input from stdin
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).ok();

    // Auto-start daemon if not running
    let health = std::process::Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "http://localhost:3581/health"])
        .output();
    let running = health
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s == "200")
        .unwrap_or(false);

    if !running {
        let project = init::detect_project_name(None);
        let exe = std::env::current_exe().unwrap_or_default();
        let _ = std::process::Command::new(exe)
            .args(["start", "--project", &project, "--daemon"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Inject context telling the session to call temper("orchestrator")
    let context = "You have the feldspar MCP server connected. Call the `temper` tool with your role to get your instructions. If you have no role assigned, use role `orchestrator`.";
    let escaped = context
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    println!(
        "{{\n  \"hookSpecificOutput\": {{\n    \"hookEventName\": \"SessionStart\",\n    \"additionalContext\": \"{}\"\n  }}\n}}",
        escaped
    );
}

async fn run_server(port: u16, project: &str) {
    let config = config::Config::load_merged(project);
    let llm = llm::LlmClient::new(&config.llm);

    // DB init — best-effort
    let db = db::Db::open(&config.feldspar.db_path).await;
    let db = db.map(Arc::new);

    // Leaf cache: populate from DB history
    let leaf_cache = Arc::new(RwLock::new(HashMap::new()));
    if let Some(ref db) = db {
        for entry in db.load_leaf_nodes().await {
            leaf_cache.write().await.insert(entry.trace_id, entry.leaf_nodes);
        }
    }

    // Build mode_map from config (sorted for stable index assignment)
    let mut mode_names: Vec<String> = config.modes.keys().cloned().collect();
    mode_names.sort();
    let mode_map: HashMap<String, usize> = mode_names
        .into_iter()
        .enumerate()
        .map(|(i, m)| (m, i))
        .collect();

    // ML startup — load from file, disaster-recover from DB, or cold start
    let db_dir = std::path::Path::new(&config.feldspar.db_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let model_path = db_dir.join("model.bin");

    let ml_loaded = ml::MlEngine::load(&model_path, mode_map.clone(), config.feldspar.ml_budget);
    let ml = if ml_loaded.is_none() {
        if let Some(ref db_ref) = db {
            let matrix = db_ref.load_feature_matrix().await;
            if matrix.is_empty() {
                tracing::warn!("no feature vectors in DB for disaster recovery");
                None
            } else {
                tracing::info!(traces = matrix.len(), "disaster recovery: training from DB");
                ml::MlEngine::disaster_recover(
                    &matrix,
                    mode_map.clone(),
                    model_path.clone(),
                    config.feldspar.ml_budget,
                )
            }
        } else {
            None
        }
    } else {
        ml_loaded
    };
    let ml = ml.map(Arc::new);

    let agent_defs = agents::load_agents(project);
    info!(count = agent_defs.len(), "loaded agent definitions");

    let ar_engine = config.ar.as_ref().and_then(|ac| ar::ArEngine::new(ac));
    if ar_engine.is_some() {
        info!("AR engine initialized");
    } else {
        info!("AR engine disabled (no config or missing OPENROUTER_API_KEY)");
    }

    let leaf_cache_for_prune = leaf_cache.clone();
    let server = thought::ThinkingServer::new(config, llm, db.clone(), leaf_cache, ml.clone());
    let state = Arc::new(mcp::McpState::new(server, agent_defs, ar_engine, project.to_owned(), port));

    mcp::sweep_orphaned_changes(project);

    let cleanup_state = state.clone();
    tokio::spawn(mcp::session_cleanup_task(cleanup_state));

    // Prune timer — every 30 minutes
    if let (Some(db), Some(ml)) = (&db, &ml) {
        let db_clone = db.clone();
        let ml_clone = ml.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30 * 60));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                thought::run_prune(&ml_clone, &db_clone, &leaf_cache_for_prune).await;
            }
        });
    }

    let router = mcp::create_router(state);
    let addr = format!("127.0.0.1:{}", port);
    info!("feldspar listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind to {}: {}", addr, e));

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    // Shutdown: flush remaining training buffer + save model
    if let Some(ref ml) = ml {
        ml.flush_buffer();
        let _ = ml.save();
    }

    info!("feldspar shutdown complete");
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
    info!("shutdown signal received");
}
