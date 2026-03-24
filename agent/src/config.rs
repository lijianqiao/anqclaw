use anyhow::{Context, Result};
use secrecy::SecretString;
use serde::Deserialize;

// ─── Default value helpers ────────────────────────────────────────────────────

fn default_app_name() -> String {
    "anq-agent".to_string()
}
fn default_workspace() -> String {
    "./workspace".to_string()
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_allow_from() -> Vec<String> {
    vec![]
}
fn default_llm_provider() -> String {
    "anthropic".to_string()
}
fn default_llm_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
fn default_base_url() -> String {
    String::new()
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_temperature() -> f32 {
    0.7
}
fn default_db_path() -> String {
    "./data/memory.db".to_string()
}
fn default_history_limit() -> u32 {
    20
}
fn default_search_limit() -> u32 {
    5
}
fn default_heartbeat_enabled() -> bool {
    false
}
fn default_interval_minutes() -> u32 {
    30
}
fn default_notify_channel() -> String {
    "feishu".to_string()
}
fn default_notify_chat_id() -> String {
    String::new()
}
fn default_shell_enabled() -> bool {
    true
}
fn default_shell_allowed_commands() -> Vec<String> {
    vec![
        "ls".to_string(),
        "cat".to_string(),
        "grep".to_string(),
        "find".to_string(),
        "date".to_string(),
        "curl".to_string(),
    ]
}
fn default_shell_timeout_secs() -> u32 {
    30
}
fn default_web_fetch_enabled() -> bool {
    true
}
fn default_web_fetch_timeout_secs() -> u32 {
    10
}
fn default_web_fetch_max_bytes() -> u64 {
    102400
}
fn default_file_enabled() -> bool {
    true
}
fn default_file_access_dir() -> String {
    "./workspace".to_string()
}
fn default_memory_tool_enabled() -> bool {
    true
}
fn default_memory_tool_search_limit() -> u32 {
    5
}
fn default_max_tool_rounds() -> u32 {
    10
}
fn default_system_prompt_file() -> String {
    String::new()
}

// ─── Raw deserialization structs (secrets as plain String) ────────────────────

/// Intermediate struct for TOML deserialization — secrets stored as plain String
/// so we can inspect them for `${VAR}` substitution before wrapping in SecretString.
#[derive(Deserialize)]
struct RawFeishuSection {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_allow_from")]
    pub allow_from: Vec<String>,
}

#[derive(Deserialize)]
struct RawLlmSection {
    #[serde(default = "default_llm_provider")]
    pub provider: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

#[derive(Deserialize)]
struct RawAppConfig {
    pub app: AppSection,
    pub feishu: RawFeishuSection,
    pub llm: RawLlmSection,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub memory: MemorySection,
    #[serde(default)]
    pub heartbeat: HeartbeatSection,
    #[serde(default)]
    pub agent: AgentSection,
}

// ─── Public config structs ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AppSection {
    #[serde(default = "default_app_name")]
    pub name: String,
    #[serde(default = "default_workspace")]
    pub workspace: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for AppSection {
    fn default() -> Self {
        Self {
            name: default_app_name(),
            workspace: default_workspace(),
            log_level: default_log_level(),
        }
    }
}

/// Feishu integration settings.
/// `app_secret` is wrapped in `SecretString` after env-var resolution.
#[derive(Debug)]
pub struct FeishuSection {
    pub app_id: String,
    pub app_secret: SecretString,
    pub allow_from: Vec<String>,
}

/// LLM provider settings.
/// `api_key` is wrapped in `SecretString` after env-var resolution.
#[derive(Debug)]
pub struct LlmSection {
    pub provider: String,
    pub model: String,
    pub api_key: SecretString,
    pub base_url: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Deserialize)]
pub struct ToolsSection {
    #[serde(default = "default_shell_enabled")]
    pub shell_enabled: bool,
    #[serde(default = "default_shell_allowed_commands")]
    pub shell_allowed_commands: Vec<String>,
    #[serde(default = "default_shell_timeout_secs")]
    pub shell_timeout_secs: u32,

    #[serde(default = "default_web_fetch_enabled")]
    pub web_fetch_enabled: bool,
    #[serde(default = "default_web_fetch_timeout_secs")]
    pub web_fetch_timeout_secs: u32,
    #[serde(default = "default_web_fetch_max_bytes")]
    pub web_fetch_max_bytes: u64,

    #[serde(default = "default_file_enabled")]
    pub file_enabled: bool,
    #[serde(default = "default_file_access_dir")]
    pub file_access_dir: String,

    #[serde(default = "default_memory_tool_enabled")]
    pub memory_tool_enabled: bool,
    #[serde(default = "default_memory_tool_search_limit")]
    pub memory_tool_search_limit: u32,
}

impl Default for ToolsSection {
    fn default() -> Self {
        Self {
            shell_enabled: default_shell_enabled(),
            shell_allowed_commands: default_shell_allowed_commands(),
            shell_timeout_secs: default_shell_timeout_secs(),
            web_fetch_enabled: default_web_fetch_enabled(),
            web_fetch_timeout_secs: default_web_fetch_timeout_secs(),
            web_fetch_max_bytes: default_web_fetch_max_bytes(),
            file_enabled: default_file_enabled(),
            file_access_dir: default_file_access_dir(),
            memory_tool_enabled: default_memory_tool_enabled(),
            memory_tool_search_limit: default_memory_tool_search_limit(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct MemorySection {
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    #[serde(default = "default_search_limit")]
    pub search_limit: u32,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            history_limit: default_history_limit(),
            search_limit: default_search_limit(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatSection {
    #[serde(default = "default_heartbeat_enabled")]
    pub enabled: bool,
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u32,
    #[serde(default = "default_notify_channel")]
    pub notify_channel: String,
    #[serde(default = "default_notify_chat_id")]
    pub notify_chat_id: String,
}

impl Default for HeartbeatSection {
    fn default() -> Self {
        Self {
            enabled: default_heartbeat_enabled(),
            interval_minutes: default_interval_minutes(),
            notify_channel: default_notify_channel(),
            notify_chat_id: default_notify_chat_id(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AgentSection {
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    #[serde(default = "default_system_prompt_file")]
    pub system_prompt_file: String,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            max_tool_rounds: default_max_tool_rounds(),
            system_prompt_file: default_system_prompt_file(),
        }
    }
}

// ─── Top-level config ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppConfig {
    pub app: AppSection,
    pub feishu: FeishuSection,
    pub llm: LlmSection,
    pub tools: ToolsSection,
    pub memory: MemorySection,
    pub heartbeat: HeartbeatSection,
    pub agent: AgentSection,
}

// ─── Env-var resolution ───────────────────────────────────────────────────────

/// If `value` looks like `${VAR_NAME}`, read the environment variable `VAR_NAME`.
/// Otherwise return the value unchanged.
/// Returns an error if the referenced env var is not set.
fn resolve_env(value: &str, field_name: &str) -> Result<String> {
    if let Some(inner) = value.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(inner).with_context(|| {
            format!(
                "Config field `{}` references env var `{}` which is not set",
                field_name, inner
            )
        })
    } else {
        Ok(value.to_string())
    }
}

// ─── AppConfig::load ──────────────────────────────────────────────────────────

impl AppConfig {
    /// Load configuration from a TOML file.
    ///
    /// Steps:
    /// 1. Read the file from disk.
    /// 2. Parse into raw intermediate structs (secrets as plain `String`).
    /// 3. Resolve `${ENV_VAR}` placeholders for `llm.api_key` and `feishu.app_secret`.
    /// 4. Wrap resolved secret strings in `SecretString`.
    pub fn load(path: &str) -> Result<Self> {
        let raw_text =
            std::fs::read_to_string(path).with_context(|| format!("Cannot read config file: {}", path))?;

        let raw: RawAppConfig =
            toml::from_str(&raw_text).with_context(|| format!("Failed to parse config file: {}", path))?;

        // Resolve env-var placeholders for sensitive fields
        let api_key_str = resolve_env(&raw.llm.api_key, "llm.api_key")?;
        let app_secret_str = resolve_env(&raw.feishu.app_secret, "feishu.app_secret")?;

        Ok(AppConfig {
            app: raw.app,
            feishu: FeishuSection {
                app_id: raw.feishu.app_id,
                app_secret: SecretString::new(app_secret_str.into()),
                allow_from: raw.feishu.allow_from,
            },
            llm: LlmSection {
                provider: raw.llm.provider,
                model: raw.llm.model,
                api_key: SecretString::new(api_key_str.into()),
                base_url: raw.llm.base_url,
                max_tokens: raw.llm.max_tokens,
                temperature: raw.llm.temperature,
            },
            tools: raw.tools,
            memory: raw.memory,
            heartbeat: raw.heartbeat,
            agent: raw.agent,
        })
    }
}
