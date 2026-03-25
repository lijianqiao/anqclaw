mod agent;
mod channel;
mod config;
mod gateway;
mod heartbeat;
mod llm;
mod memory;
mod tool;
mod types;

use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::channel::feishu::FeishuChannel;
use crate::gateway::Gateway;
use crate::heartbeat::Heartbeat;
use crate::llm::create_llm_client;
use crate::memory::MemoryStore;
use crate::tool::ToolRegistry;

// ─── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "anqclaw", version, about = "Lightweight personal AI assistant")]
struct Cli {
    /// Path to the configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

// ─── Main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // 1. Load configuration
    let config = Arc::new(config::AppConfig::load(&cli.config)?);

    // 2. Initialize tracing (JSON to stderr)
    let env_filter =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(&config.app.log_level))?;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json().with_writer(std::io::stderr))
        .init();

    tracing::info!(name = config.app.name.as_str(), "anqclaw starting");

    // 3. Initialize MemoryStore (SQLite)
    let memory = Arc::new(MemoryStore::new(&config.memory.db_path).await?);
    tracing::info!(db = config.memory.db_path.as_str(), "memory store initialized");

    // 4. Create LLM client
    let llm = create_llm_client(&config.llm);
    tracing::info!(
        provider = config.llm.provider.as_str(),
        model = config.llm.model.as_str(),
        "LLM client created"
    );

    // 5. Create ToolRegistry
    let tools = Arc::new(ToolRegistry::new(&config.tools, memory.clone()));

    // 6. Create AgentCore
    let agent = Arc::new(AgentCore::new(llm, tools, memory.clone(), config.clone()));

    // 7. Create channels
    let feishu_channel: Arc<dyn Channel> = Arc::new(FeishuChannel::new(&config.feishu));
    let channels: Vec<Arc<dyn Channel>> = vec![feishu_channel];

    // 8. Create Gateway
    let gateway = Gateway::new(channels.clone(), agent.clone(), memory.clone(), config.clone());

    // 9. Spawn Gateway
    let gw = gateway.clone();
    let gateway_handle = tokio::spawn(async move {
        if let Err(e) = gw.run().await {
            tracing::error!(error = %e, "gateway exited with error");
        }
    });

    // 10. Spawn Heartbeat (if enabled)
    let heartbeat_handle = if config.heartbeat.enabled {
        let hb = Heartbeat::new(
            &config.heartbeat,
            agent.clone(),
            memory.clone(),
            channels.clone(),
            &config.app.workspace,
        );
        tracing::info!(
            interval_mins = config.heartbeat.interval_minutes,
            "heartbeat enabled"
        );
        Some(tokio::spawn(async move {
            if let Err(e) = hb.run().await {
                tracing::error!(error = %e, "heartbeat exited with error");
            }
        }))
    } else {
        tracing::info!("heartbeat disabled");
        None
    };

    tracing::info!("anqclaw started — press Ctrl+C to stop");

    // 11. Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received, stopping...");

    // 12. Graceful shutdown
    // Cancel gateway and heartbeat tasks
    gateway_handle.abort();
    if let Some(hb) = heartbeat_handle {
        hb.abort();
    }

    // Wait briefly for in-flight message processing to complete
    tracing::info!("waiting for in-flight tasks to finish (max 30s)...");
    let shutdown_timeout = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        async {
            // Gateway and heartbeat are aborted; give a moment for in-progress tasks
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        },
    )
    .await;

    if shutdown_timeout.is_err() {
        tracing::warn!("shutdown timeout — forcing exit");
    }

    // Flush SQLite pending writes
    memory.close().await;
    tracing::info!("memory store closed");

    tracing::info!("anqclaw stopped — goodbye!");
    Ok(())
}
