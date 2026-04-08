mod analyzers;
mod config;
mod db;
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
    },
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
        Commands::Start { daemon, port } => {
            if daemon {
                let exe = std::env::current_exe().expect("failed to get executable path");
                std::process::Command::new(exe)
                    .args(["start", "--port", &port.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("failed to spawn daemon");
                println!("feldspar daemon started on port {}", port);
                return;
            }

            run_server(port).await;
        }
    }
}

async fn run_server(port: u16) {
    let config = config::Config::load("config/feldspar.toml", "config/principles.toml");
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

    let leaf_cache_for_prune = leaf_cache.clone();
    let server = thought::ThinkingServer::new(config, llm, db.clone(), leaf_cache, ml.clone());
    let state = Arc::new(mcp::McpState::new(server));

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
