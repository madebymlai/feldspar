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

    // Leaf cache + ML startup from DB history
    let leaf_cache = Arc::new(RwLock::new(HashMap::new()));
    if let Some(ref db) = db {
        let _traces = db.load_traces().await;
        // ml.bulk_train(traces) — wired when ML is implemented

        for entry in db.load_leaf_nodes().await {
            leaf_cache.write().await.insert(entry.trace_id, entry.leaf_nodes);
        }
    }

    let server = thought::ThinkingServer::new(config, llm, db.clone(), leaf_cache);
    let state = Arc::new(mcp::McpState::new(server));

    let cleanup_state = state.clone();
    tokio::spawn(mcp::session_cleanup_task(cleanup_state));

    // Prune timer — every 30 minutes
    if let Some(ref db) = db {
        let db_clone = db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30 * 60));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                // let ids = ml.identify_low_value_traces();
                // db_clone.prune(&ids).await;
                // evict from leaf_cache
                let _ = &db_clone; // keep alive until ML wires in
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
