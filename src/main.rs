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
            let api_key = init::prompt_api_key();
            let project_dir = std::env::current_dir().expect("failed to get cwd");
            init::run_init(&project_name, &project_dir, &api_key).expect("init failed");
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
    let project = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "default".into());

    let diff = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    if diff.trim().is_empty() {
        return;
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let artifacts_dir = std::path::PathBuf::from(home)
        .join(".feldspar/data")
        .join(&project)
        .join("artifacts/build");

    if std::fs::create_dir_all(&artifacts_dir).is_err() {
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let entry = format!(
        "\n[[changes]]\ntimestamp = {}\ndiff = \"\"\"\n{}\"\"\"\n",
        timestamp,
        diff.trim()
    );

    let path = artifacts_dir.join("changes.toml");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = file.write_all(entry.as_bytes());
    }
}

fn session_start() {
    // Best-effort session start hook — currently a no-op placeholder
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
    let state = Arc::new(mcp::McpState::new(server, agent_defs, ar_engine, project.to_owned()));

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
