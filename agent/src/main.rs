mod agent;
mod audit;
mod channel;
mod cli;
mod config;
mod gateway;
mod heartbeat;
mod llm;
mod memory;
mod metrics;
mod paths;
mod scheduler;
mod skill;
mod token;
mod tool;
mod types;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::channel::cli::CliChannel;
use crate::channel::feishu::FeishuChannel;
use crate::channel::http::HttpChannel;
use crate::gateway::Gateway;
use crate::heartbeat::Heartbeat;
use crate::llm::create_llm_client;
use crate::memory::MemoryStore;
use crate::paths::{anqclaw_home, ensure_dirs, find_config, resolve_path};
use crate::scheduler::Scheduler;
use crate::tool::ToolRegistry;

// ─── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "anqclaw", version, about = "Lightweight personal AI assistant")]
struct Cli {
    /// Path to the configuration file (overrides auto-detection)
    #[arg(short, long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the Feishu WebSocket service (default if no subcommand)
    Serve,
    /// Chat with the assistant via CLI
    Chat {
        /// Single-shot message (omit for interactive REPL)
        message: Option<String>,
    },
    /// Interactive onboarding wizard
    Onboard,
    /// Configuration management
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Export a session to JSON
    Export {
        /// The chat_id to export
        chat_id: String,
        /// Output file path (defaults to <chat_id>.json)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Import a session from JSON
    Import {
        /// Path to the JSON file
        file: String,
    },
    /// Session management
    Sessions {
        #[command(subcommand)]
        action: Option<SessionAction>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show current configuration (secrets masked)
    Show,
    /// Validate configuration and check connectivity
    Validate,
}

#[derive(Subcommand)]
enum SessionAction {
    /// Clean sessions older than a given duration (e.g. 30d, 24h)
    Clean {
        /// Duration threshold, e.g. "30d", "7d", "24h"
        #[arg(long)]
        before: String,
    },
    /// Delete a specific session by chat_id
    Delete {
        /// The chat_id to delete
        chat_id: String,
    },
}

// ─── Main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Serve);

    match command {
        Commands::Onboard => cli::onboard::run_onboard(),
        Commands::Config { action } => match action {
            ConfigAction::Show => cli::config_cmd::run_show(cli.config.as_deref()),
            ConfigAction::Validate => cli::config_cmd::run_validate(cli.config.as_deref()),
        },
        Commands::Chat { message } => run_chat(cli.config, message).await,
        Commands::Serve => run_serve(cli.config).await,
        Commands::Export { chat_id, output } => {
            run_session_cmd(cli.config, |memory| async move {
                cli::session_cmd::run_export(&memory, &chat_id, output.as_deref()).await
            })
            .await
        }
        Commands::Import { file } => {
            run_session_cmd(cli.config, |memory| async move {
                cli::session_cmd::run_import(&memory, &file).await
            })
            .await
        }
        Commands::Sessions { action } => {
            run_session_cmd(cli.config, |memory| async move {
                match action {
                    None => cli::session_cmd::run_list(&memory).await,
                    Some(SessionAction::Clean { before }) => {
                        cli::session_cmd::run_clean(&memory, &before).await
                    }
                    Some(SessionAction::Delete { chat_id }) => {
                        cli::session_cmd::run_delete(&memory, &chat_id).await
                    }
                }
            })
            .await
        }
    }
}

// ─── Shared bootstrap ──────────────────────────────────────────────────────

struct Bootstrap {
    config: Arc<config::AppConfig>,
    memory: Arc<MemoryStore>,
    agent: Arc<AgentCore>,
    #[allow(dead_code)]
    skill_registry: Option<Arc<skill::SkillRegistry>>,
    #[allow(dead_code)]
    home: std::path::PathBuf,
}

async fn bootstrap(cli_config: Option<String>) -> anyhow::Result<Bootstrap> {
    let home = anqclaw_home();
    ensure_dirs(&home)?;

    // Find and load configuration
    let config_path = find_config(cli_config.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "No config file found. Searched:\n  \
             1. --config <path>\n  \
             2. $ANQCLAW_CONFIG\n  \
             3. ./config.toml\n  \
             4. {}\n\n\
             Run `anqclaw onboard` to create one.",
            home.join("config.toml").display()
        )
    })?;

    let config_str = config_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "config path contains invalid UTF-8: {}",
            config_path.display()
        )
    })?;
    let mut config = config::AppConfig::load(config_str)?;

    // Resolve relative paths against anqclaw home
    config.app.workspace = resolve_path(&home, &config.app.workspace)
        .to_string_lossy()
        .into_owned();
    config.memory.db_path = resolve_path(&home, &config.memory.db_path)
        .to_string_lossy()
        .into_owned();
    config.tools.file_access_dir = resolve_path(&home, &config.tools.file_access_dir)
        .to_string_lossy()
        .into_owned();
    if !config.app.log_file.is_empty() {
        config.app.log_file = resolve_path(&home, &config.app.log_file)
            .to_string_lossy()
            .into_owned();
    }

    let config = Arc::new(config);

    // Initialize tracing
    let env_filter =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(&config.app.log_level))?;

    // stderr layer (always on)
    let stderr_layer = fmt::layer().json().with_writer(std::io::stderr);

    // Optional file layer
    let file_layer = if !config.app.log_file.is_empty() {
        let log_path = std::path::Path::new(&config.app.log_file);
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file_appender = tracing_appender::rolling::daily(
            log_path.parent().unwrap_or(std::path::Path::new(".")),
            log_path
                .file_name()
                .unwrap_or(std::ffi::OsStr::new("anqclaw.log")),
        );
        Some(fmt::layer().json().with_writer(file_appender))
    } else {
        None
    };

    // Use try_init to avoid double-init panic when tests call this
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init();

    tracing::info!(
        name = config.app.name.as_str(),
        home = %home.display(),
        config = %config_path.display(),
        "anqclaw starting"
    );

    // Initialize MemoryStore
    let memory = Arc::new(MemoryStore::new(&config.memory.db_path).await?);
    tracing::info!(
        db = config.memory.db_path.as_str(),
        "memory store initialized"
    );

    // Create LLM client
    let llm = create_llm_client(&config.llm)?;
    tracing::info!(
        provider = config.llm.provider.as_str(),
        model = config.llm.model.as_str(),
        "LLM client created"
    );

    // Create fallback LLM client if configured
    let fallback_llm = if !config.agent.fallback_profile.is_empty() {
        if let Some(fallback_config) = config.llm_profiles.get(&config.agent.fallback_profile) {
            Some(create_llm_client(fallback_config)?)
        } else {
            tracing::warn!(
                profile = config.agent.fallback_profile.as_str(),
                "fallback LLM profile not found, ignoring"
            );
            None
        }
    } else {
        None
    };

    // Initialize audit logger
    let audit_logger = if config.audit.enabled {
        let audit_path = resolve_path(&home, &config.audit.log_file)
            .to_string_lossy()
            .into_owned();
        match audit::AuditLogger::new(&audit_path) {
            Ok(logger) => {
                tracing::info!(path = audit_path.as_str(), "audit logging enabled");
                Some(Arc::new(logger))
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to initialize audit logger, continuing without");
                None
            }
        }
    } else {
        None
    };

    // Scan skills directory
    let skill_registry = if config.skills.enabled {
        let skills_dir = resolve_path(&home, &config.skills.skills_dir);
        let registry = Arc::new(skill::SkillRegistry::scan(&skills_dir));
        tracing::info!(
            dir = %skills_dir.display(),
            count = registry.list().len(),
            "skills scanned"
        );
        Some(registry)
    } else {
        tracing::info!("skills disabled");
        None
    };

    // Create ToolRegistry & AgentCore
    let llm_profile_names: Vec<String> = config.llm_profiles.keys().cloned().collect();
    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        skill_registry.clone(),
        llm_profile_names,
        Some(&config.skills),
    ));
    let agent = Arc::new(AgentCore::new(
        llm,
        fallback_llm,
        tools,
        memory.clone(),
        config.clone(),
        audit_logger,
        skill_registry.clone(),
    ).await);

    Ok(Bootstrap {
        config,
        memory,
        agent,
        skill_registry,
        home,
    })
}

/// Lightweight bootstrap for session management commands that only need MemoryStore.
async fn run_session_cmd<F, Fut>(cli_config: Option<String>, f: F) -> anyhow::Result<()>
where
    F: FnOnce(Arc<MemoryStore>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let home = anqclaw_home();
    ensure_dirs(&home)?;

    let config_path = find_config(cli_config.as_deref()).ok_or_else(|| {
        anyhow::anyhow!("No config file found. Run `anqclaw onboard` to create one.")
    })?;

    let config_str = config_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "config path contains invalid UTF-8: {}",
            config_path.display()
        )
    })?;
    let mut config = config::AppConfig::load(config_str)?;
    config.memory.db_path = resolve_path(&home, &config.memory.db_path)
        .to_string_lossy()
        .into_owned();

    let memory = Arc::new(MemoryStore::new(&config.memory.db_path).await?);
    let result = f(memory.clone()).await;
    memory.close().await;
    result
}

// ─── `anqclaw serve` ────────────────────────────────────────────────────────

async fn run_serve(cli_config: Option<String>) -> anyhow::Result<()> {
    let bs = bootstrap(cli_config).await?;

    // Create channels (feishu is optional)
    let mut channels: Vec<Arc<dyn Channel>> = Vec::new();

    // Spawn skills hot-reload watcher (must be held alive)
    let _skill_watcher = if let Some(ref registry) = bs.skill_registry {
        match skill::spawn_skill_watcher(registry.clone()) {
            Ok(w) => {
                tracing::info!("skills hot-reload watcher started");
                Some(w)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start skills hot-reload watcher");
                None
            }
        }
    } else {
        None
    };

    if let Some(ref feishu_cfg) = bs.config.feishu {
        let feishu_channel: Arc<dyn Channel> = Arc::new(FeishuChannel::new(feishu_cfg)?);
        channels.push(feishu_channel);
        tracing::info!("feishu channel enabled");
    } else {
        tracing::info!("feishu channel not configured — skipping");
    }

    // Create & spawn Gateway
    let app_metrics = Arc::new(metrics::Metrics::new());

    // HTTP channel (optional)
    if bs.config.http_channel.enabled {
        let http_channel: Arc<dyn Channel> = Arc::new(HttpChannel::new(
            &bs.config.http_channel,
            Some(bs.agent.clone()),
            Some(bs.memory.clone()),
            Some(bs.config.clone()),
            Some(app_metrics.clone()),
        ));
        channels.push(http_channel);
        tracing::info!(bind = %bs.config.http_channel.bind, "http channel enabled");
    }

    if channels.is_empty() {
        tracing::warn!("no channels configured — only heartbeat/scheduler will run (if enabled)");
    }

    let gateway = Gateway::new(
        channels.clone(),
        bs.agent.clone(),
        bs.memory.clone(),
        bs.config.clone(),
        app_metrics,
    );
    let gw = gateway.clone();
    let gateway_handle = tokio::spawn(async move {
        if let Err(e) = gw.run().await {
            tracing::error!(error = %e, "gateway exited with error");
        }
    });

    // Spawn Heartbeat (if enabled)
    let heartbeat_handle = if bs.config.heartbeat.enabled {
        let hb = Heartbeat::new(
            &bs.config.heartbeat,
            bs.agent.clone(),
            bs.memory.clone(),
            channels.clone(),
            &bs.config.app.workspace,
        );
        tracing::info!(
            interval_mins = bs.config.heartbeat.interval_minutes,
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

    // Spawn Scheduler (if enabled)
    let scheduler_handle = if bs.config.scheduler.enabled {
        let sched = Scheduler::new(
            &bs.config.scheduler.tasks,
            bs.agent.clone(),
            bs.memory.clone(),
            channels.clone(),
            &bs.home,
        );
        tracing::info!(task_count = sched.task_count(), "scheduler enabled");
        Some(tokio::spawn(async move {
            if let Err(e) = sched.run().await {
                tracing::error!(error = %e, "scheduler exited with error");
            }
        }))
    } else {
        tracing::info!("scheduler disabled");
        None
    };

    tracing::info!("anqclaw serve started — press Ctrl+C to stop");

    // Wait for shutdown
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received, stopping...");

    gateway_handle.abort();
    if let Some(hb) = heartbeat_handle {
        hb.abort();
    }
    if let Some(sh) = scheduler_handle {
        sh.abort();
    }

    tracing::info!("waiting for in-flight tasks to finish (max 30s)...");
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::time::sleep(std::time::Duration::from_millis(500)),
    )
    .await;

    bs.memory.close().await;
    tracing::info!("anqclaw stopped — goodbye!");
    Ok(())
}

// ─── `anqclaw chat` ────────────────────────────────────────────────────────

async fn run_chat(cli_config: Option<String>, message: Option<String>) -> anyhow::Result<()> {
    let bs = bootstrap(cli_config).await?;

    let is_single_shot = message.is_some();

    let cli_channel: Arc<dyn Channel> = Arc::new(CliChannel::new(message));
    let channels: Vec<Arc<dyn Channel>> = vec![cli_channel];

    let app_metrics = Arc::new(metrics::Metrics::new());
    let gateway = Gateway::new(
        channels,
        bs.agent.clone(),
        bs.memory.clone(),
        bs.config.clone(),
        app_metrics,
    );

    if is_single_shot {
        // Single-shot: run gateway, wait for it to complete, exit
        let gw = gateway.clone();
        let handle = tokio::spawn(async move { gw.run().await });

        // Give the single message time to process, then shut down
        // The gateway will exit naturally when the CLI channel finishes
        tokio::select! {
            result = handle => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "gateway error");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("interrupted");
            }
        }
    } else {
        // Interactive REPL mode
        println!("\x1b[1m🤖 anqclaw chat\x1b[0m — 输入 /exit 退出\n");

        let gw = gateway.clone();
        let handle = tokio::spawn(async move { gw.run().await });

        tokio::select! {
            result = handle => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "gateway error");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n\x1b[33m👋 再见！\x1b[0m");
            }
        }
    }

    bs.memory.close().await;
    Ok(())
}
