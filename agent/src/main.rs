//! @file
//! @author lijianqiao
//! @since 2026-03-31
//! @brief 负责 anqclaw 的启动、装配与命令行入口。

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
mod session;
mod skill;
mod token;
mod tool;
mod types;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::channel::cli::CliChannel;
use crate::channel::feishu::FeishuChannel;
use crate::channel::http::HttpChannel;
use crate::gateway::Gateway;
use crate::heartbeat::build_heartbeat_task;
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

fn bundled_skills_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("skills")))
}

fn collect_skill_sources(
    home: &Path,
    workspace: &Path,
    skills_dir: &str,
) -> Vec<skill::SkillSource> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();

    let mut push_source = |name: &str, dir: PathBuf| {
        if seen.insert(dir.clone()) {
            sources.push(skill::SkillSource::new(name, dir));
        }
    };

    if let Some(dir) = bundled_skills_dir() {
        push_source("bundled", dir);
    }
    push_source("user", home.join("skills"));
    push_source("workspace", resolve_path(workspace, skills_dir));

    sources
}

async fn bootstrap(cli_config: Option<String>) -> anyhow::Result<Bootstrap> {
    let home = anqclaw_home();
    ensure_dirs(&home)?;

    // Find and load configuration
    let config_path = find_config(cli_config.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "No config file found / 未找到配置文件. Searched:\n  \
             1. --config <path>\n  \
             2. $ANQCLAW_CONFIG\n  \
             3. ./config.toml\n  \
             4. {}\n\n\
             Run `anqclaw onboard` to create one. / 运行 `anqclaw onboard` 创建配置文件。",
            home.join("config.toml").display()
        )
    })?;

    let config_str = config_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "config path contains invalid UTF-8 / 配置路径包含无效 UTF-8: {}",
            config_path.display()
        )
    })?;
    let mut config = config::AppConfig::load(config_str)?;
    config.resolve_paths_against(&home);

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
        "anqclaw starting / anqclaw 启动中"
    );

    // Initialize MemoryStore
    let memory = Arc::new(MemoryStore::new(&config.memory.db_path).await?);
    tracing::info!(
        db = config.memory.db_path.as_str(),
        "memory store initialized / 内存存储已初始化"
    );

    // Create LLM client
    let llm = create_llm_client(&config.llm)?;
    tracing::info!(
        provider = config.llm.provider.as_str(),
        model = config.llm.model.as_str(),
        "LLM client created / LLM 客户端已创建"
    );

    // Create fallback LLM client if configured
    let fallback_llm = if !config.agent.fallback_profile.is_empty() {
        if let Some(fallback_config) = config.llm_profiles.get(&config.agent.fallback_profile) {
            Some(create_llm_client(fallback_config)?)
        } else {
            tracing::warn!(
                profile = config.agent.fallback_profile.as_str(),
                "fallback LLM profile not found, ignoring / 备用 LLM 配置未找到，已忽略"
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
                tracing::info!(
                    path = audit_path.as_str(),
                    "audit logging enabled / 审计日志已启用"
                );
                Some(Arc::new(logger))
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to initialize audit logger, continuing without / 审计日志初始化失败，继续运行");
                None
            }
        }
    } else {
        None
    };

    // Scan skills directories
    let skill_registry = if config.skills.enabled {
        let skill_sources = collect_skill_sources(
            &home,
            Path::new(&config.app.workspace),
            &config.skills.skills_dir,
        );
        let registry = Arc::new(skill::SkillRegistry::scan(
            skill_sources,
            config.skills.max_skill_file_bytes,
        ));
        tracing::info!(
            dirs = ?registry
                .sources()
                .iter()
                .map(|source| format!("{}:{}", source.name, source.dir.display()))
                .collect::<Vec<_>>(),
            count = registry.list().len(),
            max_skill_file_bytes = config.skills.max_skill_file_bytes,
            prompt_mainline = "available_skills+file_read",
            activate_skill = "compatibility_debug",
            "skills scanned / 技能已扫描"
        );
        Some(registry)
    } else {
        tracing::info!("skills disabled / 技能已禁用");
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
    let agent = Arc::new(
        AgentCore::new(
            llm,
            fallback_llm,
            tools,
            memory.clone(),
            config.clone(),
            audit_logger,
            skill_registry.clone(),
        )
        .await,
    );

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
        anyhow::anyhow!("No config file found / 未找到配置文件. Run `anqclaw onboard` to create one. / 运行 `anqclaw onboard` 创建配置文件。")
    })?;

    let config_str = config_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "config path contains invalid UTF-8 / 配置路径包含无效 UTF-8: {}",
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
                tracing::info!("skills hot-reload watcher started / 技能热重载监视器已启动");
                Some(w)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start skills hot-reload watcher / 技能热重载监视器启动失败");
                None
            }
        }
    } else {
        None
    };

    if let Some(ref feishu_cfg) = bs.config.feishu {
        let feishu_channel: Arc<dyn Channel> = Arc::new(FeishuChannel::new(feishu_cfg)?);
        channels.push(feishu_channel);
        tracing::info!("feishu channel enabled / 飞书频道已启用");
    } else {
        tracing::info!("feishu channel not configured, skipping / 飞书频道未配置，已跳过");
    }

    // Create & spawn Gateway
    let app_metrics = Arc::new(metrics::Metrics::new());

    // HTTP channel (optional) — gateway reference is injected after Gateway creation.
    let http_channel_ref: Option<Arc<HttpChannel>> = if bs.config.http_channel.enabled {
        let http_channel = Arc::new(HttpChannel::new(
            &bs.config.http_channel,
            Some(app_metrics.clone()),
        ));
        channels.push(http_channel.clone() as Arc<dyn Channel>);
        tracing::info!(bind = %bs.config.http_channel.bind, "http channel enabled / HTTP 频道已启用");
        Some(http_channel)
    } else {
        None
    };

    if channels.is_empty() {
        tracing::warn!(
            "no channels configured, only heartbeat/scheduler will run (if enabled) / 未配置频道，仅心跳/调度器运行（如已启用）"
        );
    }

    let gateway = Gateway::new(
        channels.clone(),
        bs.agent.clone(),
        bs.memory.clone(),
        bs.config.clone(),
        app_metrics,
    );

    if let Some(http_channel) = &http_channel_ref {
        http_channel.set_gateway(gateway.clone());
    }

    let gw = gateway.clone();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let gw_shutdown = shutdown.clone();
    let gateway_handle = tokio::spawn(async move {
        if let Err(e) = gw.run(gw_shutdown).await {
            tracing::error!(error = %e, "gateway exited with error / 网关退出并出错");
        }
    });

    let heartbeat_task = build_heartbeat_task(&bs.config.heartbeat, &bs.config.app.workspace);
    let scheduler_tasks = if bs.config.scheduler.enabled {
        bs.config.scheduler.tasks.clone()
    } else {
        vec![]
    };
    let scheduler_handle = if !scheduler_tasks.is_empty() || heartbeat_task.is_some() {
        if heartbeat_task.is_some() {
            tracing::info!(
                interval_mins = bs.config.heartbeat.interval_minutes,
                "heartbeat scheduled via scheduler / 心跳已通过调度器启用"
            );
        } else {
            tracing::info!("heartbeat disabled / 心跳已禁用");
        }

        let sched = Scheduler::new(
            &scheduler_tasks,
            heartbeat_task,
            bs.agent.clone(),
            bs.memory.clone(),
            channels.clone(),
            &bs.home,
        );
        let sched_shutdown = shutdown.clone();
        tracing::info!(
            task_count = sched.task_count(),
            "scheduler enabled / 调度器已启用"
        );
        Some(tokio::spawn(async move {
            if let Err(e) = sched.run(sched_shutdown).await {
                tracing::error!(error = %e, "scheduler exited with error / 调度器退出并出错");
            }
        }))
    } else {
        tracing::info!("scheduler and heartbeat disabled / 调度器与心跳均已禁用");
        None
    };

    tracing::info!(
        "anqclaw serve started, press Ctrl+C to stop / anqclaw 服务已启动，按 Ctrl+C 停止"
    );

    // Wait for shutdown
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received, stopping / 收到关闭信号，正在停止...");

    // Signal all tasks to stop accepting new work
    shutdown.cancel();

    tracing::info!(
        "waiting for in-flight tasks to finish (max 30s) / 等待进行中的任务完成（最多 30 秒）..."
    );

    // Wait for gateway and scheduler to drain gracefully, with a hard timeout
    let drain = async {
        let _ = gateway_handle.await;
        if let Some(sh) = scheduler_handle {
            let _ = sh.await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(30), drain)
        .await
        .is_err()
    {
        tracing::warn!("graceful drain timed out after 30s / 优雅排空超时（30 秒）");
    }

    bs.memory.close().await;
    tracing::info!("anqclaw stopped, goodbye / anqclaw 已停止，再见!");
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

    let chat_shutdown = tokio_util::sync::CancellationToken::new();

    if is_single_shot {
        // Single-shot: run gateway, wait for it to complete, exit
        let gw = gateway.clone();
        let gw_token = chat_shutdown.clone();
        let handle = tokio::spawn(async move { gw.run(gw_token).await });

        // Give the single message time to process, then shut down
        // The gateway will exit naturally when the CLI channel finishes
        tokio::select! {
            result = handle => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "gateway error / 网关错误");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("interrupted / 已中断");
            }
        }
    } else {
        // Interactive REPL mode
        println!("\x1b[1m🤖 anqclaw chat\x1b[0m — 输入 /exit 退出\n");

        let gw = gateway.clone();
        let gw_token = chat_shutdown.clone();
        let handle = tokio::spawn(async move { gw.run(gw_token).await });

        tokio::select! {
            result = handle => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "gateway error / 网关错误");
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
