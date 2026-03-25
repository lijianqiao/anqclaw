use std::collections::HashMap;

use anyhow::{Context, Result};
use secrecy::SecretString;
use serde::Deserialize;

// ─── Default value helpers ────────────────────────────────────────────────────

fn default_app_name() -> String {
    "anqclaw".to_string()
}
fn default_workspace() -> String {
    "workspace".to_string()
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
fn default_supports_tools() -> bool {
    true
}
fn default_db_path() -> String {
    "data/memory.db".to_string()
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
    "workspace".to_string()
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
fn default_llm_profile() -> String {
    "default".to_string()
}

// ─── Raw deserialization structs (secrets as plain String) ────────────────────

#[derive(Deserialize)]
struct RawFeishuSection {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_allow_from")]
    pub allow_from: Vec<String>,
}

/// Raw LLM profile — flat format (used in single-profile legacy mode and per-profile).
#[derive(Deserialize, Clone)]
struct RawLlmProfile {
    #[serde(default = "default_llm_provider")]
    pub provider: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Whether this model supports tool calling. Default: true.
    /// Set to false for models that error when `tools` is passed (e.g. some Ollama models).
    #[serde(default = "default_supports_tools")]
    pub supports_tools: bool,
}

/// The `[llm]` section can be one of two forms:
///
/// **Legacy (flat)** — single profile, treated as "default":
/// ```toml
/// [llm]
/// provider = "anthropic"
/// model = "claude-sonnet-4-20250514"
/// api_key = "sk-..."
/// ```
///
/// **Multi-profile** — named sub-tables:
/// ```toml
/// [llm.default]
/// provider = "anthropic"
/// ...
/// [llm.deepseek]
/// provider = "openai_compat"
/// ...
/// ```
///
/// Detection: if the TOML `[llm]` value has a `provider` key, it's legacy (flat).
/// Otherwise it's multi-profile.
fn parse_llm_profiles(llm_value: &toml::Value) -> Result<HashMap<String, RawLlmProfile>> {
    let table = llm_value
        .as_table()
        .context("[llm] must be a TOML table")?;

    // Detect: if it has a "provider" or "model" or "api_key" key at the top level,
    // treat as legacy single profile → wrap as { "default": ... }
    let is_legacy = table.contains_key("provider")
        || table.contains_key("model")
        || table.contains_key("api_key");

    if is_legacy {
        let profile: RawLlmProfile =
            toml::Value::Table(table.clone()).try_into().context("parse [llm] as flat profile")?;
        let mut map = HashMap::new();
        map.insert("default".to_string(), profile);
        Ok(map)
    } else {
        // Multi-profile: each key is a profile name
        let mut map = HashMap::new();
        for (name, value) in table {
            let profile: RawLlmProfile =
                value.clone().try_into().with_context(|| format!("parse [llm.{name}]"))?;
            map.insert(name.clone(), profile);
        }
        if map.is_empty() {
            anyhow::bail!("[llm] section is empty — at least one LLM profile is required");
        }
        Ok(map)
    }
}

/// Top-level raw config — uses `toml::Value` for `[llm]` to support both formats.
#[derive(Deserialize)]
struct RawAppConfig {
    #[serde(default)]
    pub app: AppSection,
    pub feishu: Option<RawFeishuSection>,
    pub llm: toml::Value,
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
    /// Optional log file path (relative to anqclaw home). Empty = no file logging.
    #[serde(default)]
    pub log_file: String,
}

impl Default for AppSection {
    fn default() -> Self {
        Self {
            name: default_app_name(),
            workspace: default_workspace(),
            log_level: default_log_level(),
            log_file: String::new(),
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

/// LLM provider settings (one profile).
/// `api_key` is wrapped in `SecretString` after env-var resolution.
#[derive(Debug)]
pub struct LlmSection {
    pub provider: String,
    pub model: String,
    /// May be empty for providers that don't need auth (e.g. Ollama).
    pub api_key: SecretString,
    pub base_url: String,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Whether this model supports tool calling.
    pub supports_tools: bool,
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
    /// Which LLM profile to use. Default: "default".
    #[serde(default = "default_llm_profile")]
    pub llm_profile: String,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            max_tool_rounds: default_max_tool_rounds(),
            system_prompt_file: default_system_prompt_file(),
            llm_profile: default_llm_profile(),
        }
    }
}

// ─── Top-level config ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppConfig {
    pub app: AppSection,
    /// `None` if `[feishu]` is omitted from config — Feishu channel won't start.
    pub feishu: Option<FeishuSection>,
    /// Named LLM profiles. At least one ("default") is required.
    pub llm_profiles: HashMap<String, LlmSection>,
    /// Convenience accessor: the active LLM profile (determined by `agent.llm_profile`).
    /// This is a clone from `llm_profiles` — used by legacy code that expects a single `LlmSection`.
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

/// If `value` is empty or looks like `${VAR}` but the var is unset, return empty string
/// (no error). Used for optional secrets like Ollama's api_key.
fn resolve_env_optional(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if let Some(inner) = value.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(inner).unwrap_or_default()
    } else {
        value.to_string()
    }
}

// ─── AppConfig::load ──────────────────────────────────────────────────────────

impl AppConfig {
    /// Load configuration from a TOML file.
    pub fn load(path: &str) -> Result<Self> {
        let raw_text = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read config file: {}", path))?;

        Self::load_from_str(&raw_text)
    }

    /// Parse configuration from a TOML string (useful for tests).
    pub fn load_from_str(toml_text: &str) -> Result<Self> {
        let raw: RawAppConfig = toml::from_str(toml_text).context("Failed to parse config TOML")?;

        // --- Feishu (optional) ---
        let feishu = match raw.feishu {
            Some(f) => {
                let app_secret_str = resolve_env(&f.app_secret, "feishu.app_secret")?;
                Some(FeishuSection {
                    app_id: f.app_id,
                    app_secret: SecretString::new(app_secret_str.into()),
                    allow_from: f.allow_from,
                })
            }
            None => None,
        };

        // --- LLM profiles ---
        let raw_profiles = parse_llm_profiles(&raw.llm)?;
        let mut llm_profiles = HashMap::new();

        for (name, raw_p) in &raw_profiles {
            let api_key_str =
                resolve_env_optional(&raw_p.api_key);
            llm_profiles.insert(
                name.clone(),
                LlmSection {
                    provider: raw_p.provider.clone(),
                    model: raw_p.model.clone(),
                    api_key: SecretString::new(api_key_str.into()),
                    base_url: raw_p.base_url.clone(),
                    max_tokens: raw_p.max_tokens,
                    temperature: raw_p.temperature,
                    supports_tools: raw_p.supports_tools,
                },
            );
        }

        // Determine active profile
        let active_profile_name = &raw.agent.llm_profile;
        let active_profile = llm_profiles
            .get(active_profile_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "agent.llm_profile = \"{}\" but no [llm.{}] profile found. Available: {:?}",
                    active_profile_name,
                    active_profile_name,
                    llm_profiles.keys().collect::<Vec<_>>()
                )
            })?;

        // Clone active profile into the convenience `llm` field
        let llm = LlmSection {
            provider: active_profile.provider.clone(),
            model: active_profile.model.clone(),
            api_key: SecretString::new(
                // Re-resolve because SecretString can't be cloned
                resolve_env_optional(&raw_profiles[active_profile_name].api_key).into(),
            ),
            base_url: active_profile.base_url.clone(),
            max_tokens: active_profile.max_tokens,
            temperature: active_profile.temperature,
            supports_tools: active_profile.supports_tools,
        };

        Ok(AppConfig {
            app: raw.app,
            feishu,
            llm_profiles,
            llm,
            tools: raw.tools,
            memory: raw.memory,
            heartbeat: raw.heartbeat,
            agent: raw.agent,
        })
    }
}
