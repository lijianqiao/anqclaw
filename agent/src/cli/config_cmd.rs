//! `anqclaw config show` and `anqclaw config validate` implementations.

use secrecy::ExposeSecret;

use crate::config::AppConfig;
use crate::paths::{anqclaw_home, find_config, resolve_path};

/// Mask a secret string: show first 4 chars + "****".
fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return "(empty)".to_string();
    }
    if s.len() <= 4 {
        return "****".to_string();
    }
    format!("{}****", &s[..4])
}

/// `anqclaw config show` — display current config with secrets masked.
pub fn run_show(cli_config: Option<&str>) -> anyhow::Result<()> {
    let home = anqclaw_home();

    let config_path = find_config(cli_config).ok_or_else(|| {
        anyhow::anyhow!("No config file found. Run `anqclaw onboard` to create one.")
    })?;

    println!("\x1b[1m📄 Config file:\x1b[0m {}", config_path.display());
    println!("\x1b[1m📁 Home:\x1b[0m {}", home.display());
    println!();

    let config_str = config_path.to_str().ok_or_else(|| {
        anyhow::anyhow!("config path contains invalid UTF-8: {}", config_path.display())
    })?;
    let mut config = AppConfig::load(config_str)?;

    // Resolve paths for display
    config.app.workspace = resolve_path(&home, &config.app.workspace)
        .to_string_lossy()
        .into_owned();
    config.memory.db_path = resolve_path(&home, &config.memory.db_path)
        .to_string_lossy()
        .into_owned();

    println!("\x1b[1m[app]\x1b[0m");
    println!("  name       = {}", config.app.name);
    println!("  workspace  = {}", config.app.workspace);
    println!("  log_level  = {}", config.app.log_level);
    if !config.app.log_file.is_empty() {
        println!("  log_file   = {}", config.app.log_file);
    }
    println!();

    println!("\x1b[1m[llm]\x1b[0m (active profile: {})", config.agent.llm_profile);
    println!("  provider   = {}", config.llm.provider);
    println!("  model      = {}", config.llm.model);
    println!(
        "  api_key    = {}",
        mask_secret(config.llm.api_key.expose_secret())
    );
    if !config.llm.base_url.is_empty() {
        println!("  base_url   = {}", config.llm.base_url);
    }
    println!("  max_tokens = {}", config.llm.max_tokens);
    println!("  temperature = {}", config.llm.temperature);
    println!("  supports_tools = {}", config.llm.supports_tools);
    println!("  max_retries = {}", config.llm.max_retries);

    if config.llm_profiles.len() > 1 {
        println!();
        println!("  Other profiles:");
        for (name, profile) in &config.llm_profiles {
            if name != &config.agent.llm_profile {
                println!(
                    "    {name}: {} / {} (key: {})",
                    profile.provider,
                    profile.model,
                    mask_secret(profile.api_key.expose_secret())
                );
            }
        }
    }
    println!();

    if let Some(ref feishu) = config.feishu {
        println!("\x1b[1m[channel.feishu]\x1b[0m");
        println!("  app_id     = {}", feishu.app_id);
        println!(
            "  app_secret = {}",
            mask_secret(feishu.app_secret.expose_secret())
        );
        if !feishu.allow_from.is_empty() {
            println!("  allow_from = {:?}", feishu.allow_from);
        }
    } else {
        println!("\x1b[1m[channel.feishu]\x1b[0m (not configured)");
    }
    println!();

    println!("\x1b[1m[security]\x1b[0m");
    println!("  auto_redact_secrets = {}", config.security.auto_redact_secrets);
    if !config.security.trusted_dirs.is_empty() {
        println!("  trusted_dirs        = {:?}", config.security.trusted_dirs);
    }
    if !config.security.blocked_dirs.is_empty() {
        println!("  blocked_dirs        = {:?}", config.security.blocked_dirs);
    }
    if !config.security.redact_patterns.is_empty() {
        println!("  redact_patterns     = {:?}", config.security.redact_patterns);
    }
    println!();

    println!("\x1b[1m[limits]\x1b[0m");
    println!("  max_requests_per_minute    = {}", config.limits.max_requests_per_minute);
    println!("  max_tokens_per_conversation = {}", config.limits.max_tokens_per_conversation);
    println!("  max_message_length         = {}", config.limits.max_message_length);
    println!();

    println!("\x1b[1m[memory]\x1b[0m");
    println!("  db_path       = {}", config.memory.db_path);
    println!("  history_limit = {}", config.memory.history_limit);
    println!("  search_limit  = {}", config.memory.search_limit);
    println!();

    println!("\x1b[1m[agent]\x1b[0m");
    println!("  max_tool_rounds    = {}", config.agent.max_tool_rounds);
    println!("  llm_profile        = {}", config.agent.llm_profile);
    println!("  fallback_profile   = {}", if config.agent.fallback_profile.is_empty() { "(none)" } else { &config.agent.fallback_profile });
    println!();

    println!("\x1b[1m[heartbeat]\x1b[0m");
    println!("  enabled          = {}", config.heartbeat.enabled);
    if config.heartbeat.enabled {
        println!("  interval_minutes = {}", config.heartbeat.interval_minutes);
        println!("  notify_channel   = {}", config.heartbeat.notify_channel);
    }
    println!();

    println!("\x1b[1m[audit]\x1b[0m");
    println!("  enabled  = {}", config.audit.enabled);
    if config.audit.enabled {
        println!("  log_file = {}", config.audit.log_file);
    }

    Ok(())
}

/// `anqclaw config validate` — check config, env vars, and optional connectivity.
pub fn run_validate(cli_config: Option<&str>) -> anyhow::Result<()> {
    let home = anqclaw_home();

    // 1. Check config file exists
    let config_path = match find_config(cli_config) {
        Some(p) => {
            println!("✓ Config file found: {}", p.display());
            p
        }
        None => {
            println!("✗ No config file found");
            println!("  Run `anqclaw onboard` to create one.");
            return Ok(());
        }
    };

    // 2. Parse config
    let config_str = config_path.to_str().unwrap_or("<invalid-utf8>");
    match AppConfig::load(config_str) {
        Ok(config) => {
            println!("✓ Config parsed successfully");

            // 3. Check LLM api_key
            let key = config.llm.api_key.expose_secret();
            if !key.is_empty() || config.llm.provider == "ollama" {
                println!("✓ LLM api_key present (or not needed for {})", config.llm.provider);
            } else {
                println!("✗ LLM api_key is empty — set it in config or via env var");
            }

            // 4. Check feishu (channel.feishu or legacy [feishu])
            if let Some(ref feishu) = config.feishu {
                if !feishu.app_id.is_empty() {
                    println!("✓ [channel.feishu] app_id present");
                }
                let secret = feishu.app_secret.expose_secret();
                if !secret.is_empty() {
                    println!("✓ [channel.feishu] app_secret present");
                } else {
                    println!("✗ [channel.feishu] app_secret is empty");
                }
            } else {
                println!("ℹ [channel.feishu] not configured (optional)");
            }

            // 5. Check workspace directory
            let ws = resolve_path(&home, &config.app.workspace);
            if ws.exists() {
                println!("✓ Workspace directory exists: {}", ws.display());
            } else {
                println!("⚠ Workspace directory missing: {} (will be created on first run)", ws.display());
            }

            // 6. Check data directory
            let db = resolve_path(&home, &config.memory.db_path);
            if let Some(parent) = db.parent() {
                if parent.exists() {
                    println!("✓ Data directory exists: {}", parent.display());
                } else {
                    println!("⚠ Data directory missing: {} (will be created on first run)", parent.display());
                }
            }

            println!();
            println!("\x1b[32m✓ Validation complete\x1b[0m");
        }
        Err(e) => {
            println!("✗ Failed to parse config: {e}");
        }
    }

    Ok(())
}
