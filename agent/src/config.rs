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
fn default_web_blocked_domains() -> Vec<String> {
    vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "0.0.0.0".to_string(),
        "169.254.169.254".to_string(), // Cloud metadata SSRF
        "[::1]".to_string(),
    ]
}
fn default_memory_tool_enabled() -> bool {
    true
}
fn default_memory_tool_search_limit() -> u32 {
    5
}
fn default_pdf_read_max_chars() -> u32 {
    50000
}
fn default_shell_permission_level() -> String {
    "supervised".to_string()
}
fn default_shell_blocked_commands() -> Vec<String> {
    vec![
        "rm -rf /".to_string(),
        "mkfs".to_string(),
        "dd".to_string(),
        "format".to_string(),
        "shutdown".to_string(),
        "reboot".to_string(),
        "init".to_string(),
    ]
}
fn default_trusted_dirs() -> Vec<String> {
    vec![]
}
fn default_blocked_dirs() -> Vec<String> {
    vec![]
}
fn default_auto_redact_secrets() -> bool {
    true
}
fn default_redact_patterns() -> Vec<String> {
    vec![]
}
fn default_max_requests_per_minute() -> u32 {
    20
}
fn default_max_tokens_per_conversation() -> u64 {
    50000
}
fn default_max_message_length() -> u32 {
    10000
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
fn default_max_retries() -> u32 {
    2
}
fn default_retry_delay_ms() -> u64 {
    1000
}
fn default_fallback_profile() -> String {
    String::new()
}
fn default_install_scope() -> String {
    "venv".to_string()
}
fn default_venv_path() -> String {
    ".anqclaw/envs".to_string()
}
fn default_max_consecutive_tool_errors() -> u32 {
    3
}
fn default_audit_enabled() -> bool {
    false
}
fn default_audit_log_file() -> String {
    "logs/audit.jsonl".to_string()
}
fn default_log_tool_calls() -> bool {
    true
}
fn default_log_shell_commands() -> bool {
    true
}
fn default_log_file_writes() -> bool {
    true
}
fn default_log_llm_calls() -> bool {
    false
}
fn default_skills_enabled() -> bool {
    true
}
fn default_skills_dir() -> String {
    "skills".to_string()
}
fn default_max_active_skills() -> u32 {
    3
}
fn default_session_key_strategy() -> String {
    "chat".to_string()
}
fn default_scheduler_enabled() -> bool {
    false
}
fn default_http_bind() -> String {
    "127.0.0.1:3000".to_string()
}

// ─── Audit section ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AuditSection {
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    #[serde(default = "default_audit_log_file")]
    pub log_file: String,
    #[serde(default = "default_log_tool_calls")]
    pub log_tool_calls: bool,
    #[serde(default = "default_log_shell_commands")]
    pub log_shell_commands: bool,

    #[serde(default = "default_log_file_writes")]
    pub log_file_writes: bool,
    #[serde(default = "default_log_llm_calls")]
    pub log_llm_calls: bool,
}

impl Default for AuditSection {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            log_file: default_audit_log_file(),
            log_tool_calls: default_log_tool_calls(),
            log_shell_commands: default_log_shell_commands(),
            log_file_writes: default_log_file_writes(),
            log_llm_calls: default_log_llm_calls(),
        }
    }
}

// ─── Skills section ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SkillsSection {
    #[serde(default = "default_skills_enabled")]
    pub enabled: bool,
    #[serde(default = "default_skills_dir")]
    pub skills_dir: String,
    #[serde(default = "default_max_active_skills")]
    pub max_active_skills: u32,
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self {
            enabled: default_skills_enabled(),
            skills_dir: default_skills_dir(),
            max_active_skills: default_max_active_skills(),
        }
    }
}

// ─── HTTP channel section ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HttpChannelSection {
    pub enabled: bool,
    pub bind: String,
    /// Bearer token for authentication. If empty, no auth is required.
    /// Wrapped in `SecretString` to prevent accidental logging/debug-printing.
    pub bearer_token: SecretString,
}

impl Default for HttpChannelSection {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_http_bind(),
            bearer_token: SecretString::new(String::new().into()),
        }
    }
}

/// Raw deserialization counterpart for `HttpChannelSection`.
#[derive(Deserialize, Default, Clone)]
struct RawHttpChannelSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_http_bind")]
    pub bind: String,
    #[serde(default)]
    pub bearer_token: String,
}

// ─── Scheduler section ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct SchedulerTaskConfig {
    pub name: String,
    pub cron: String,
    #[serde(default)]
    pub prompt_file: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default = "default_notify_channel")]
    pub notify_channel: String,
    #[serde(default)]
    pub notify_chat_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct SchedulerSection {
    #[serde(default = "default_scheduler_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub tasks: Vec<SchedulerTaskConfig>,
}

impl Default for SchedulerSection {
    fn default() -> Self {
        Self {
            enabled: default_scheduler_enabled(),
            tasks: vec![],
        }
    }
}

// ─── Raw deserialization structs (secrets as plain String) ────────────────────

#[derive(Deserialize)]
struct RawFeishuSection {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_allow_from")]
    pub allow_from: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawChannelSection {
    pub feishu: Option<RawFeishuSection>,
    #[serde(default)]
    pub http: Option<RawHttpChannelSection>,
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
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,
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
    let table = llm_value.as_table().context("[llm] must be a TOML table")?;

    // Detect: if it has a "provider" or "model" or "api_key" key at the top level,
    // treat as legacy single profile → wrap as { "default": ... }
    let is_legacy = table.contains_key("provider")
        || table.contains_key("model")
        || table.contains_key("api_key");

    if is_legacy {
        let profile: RawLlmProfile = toml::Value::Table(table.clone())
            .try_into()
            .context("parse [llm] as flat profile")?;
        let mut map = HashMap::new();
        map.insert("default".to_string(), profile);
        Ok(map)
    } else {
        // Multi-profile: each key is a profile name
        let mut map = HashMap::new();
        for (name, value) in table {
            let profile: RawLlmProfile = value
                .clone()
                .try_into()
                .with_context(|| format!("parse [llm.{name}]"))?;
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
    /// Legacy: `[feishu]` section (still supported for backward compatibility).
    pub feishu: Option<RawFeishuSection>,
    /// New: `[channel]` section with sub-tables.
    #[serde(default)]
    pub channel: Option<RawChannelSection>,
    pub llm: toml::Value,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub security: SecuritySection,
    #[serde(default)]
    pub limits: LimitsSection,
    #[serde(default)]
    pub memory: MemorySection,
    #[serde(default)]
    pub heartbeat: HeartbeatSection,
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub audit: AuditSection,
    #[serde(default)]
    pub skills: SkillsSection,
    #[serde(default)]
    pub scheduler: SchedulerSection,
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
    pub max_retries: u32,
    pub retry_delay_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct ToolsSection {
    #[serde(default = "default_shell_enabled")]
    pub shell_enabled: bool,
    /// Deprecated: use `shell_permission_level` + `shell_extra_allowed` instead.
    #[serde(default = "default_shell_allowed_commands")]
    pub shell_allowed_commands: Vec<String>,
    #[serde(default = "default_shell_timeout_secs")]
    pub shell_timeout_secs: u32,

    /// Shell permission level: "readonly", "supervised" (default), "full"
    #[serde(default = "default_shell_permission_level")]
    pub shell_permission_level: String,
    /// Extra commands allowed in supervised mode (appended to built-in readonly set)
    #[serde(default)]
    pub shell_extra_allowed: Vec<String>,
    /// Commands blocked even in full mode (safety net)
    #[serde(default = "default_shell_blocked_commands")]
    pub shell_blocked_commands: Vec<String>,

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

    /// Domains blocked for web_fetch (anti-SSRF). Default blocks localhost + cloud metadata.
    #[serde(default = "default_web_blocked_domains")]
    pub web_blocked_domains: Vec<String>,

    #[serde(default = "default_memory_tool_enabled")]
    pub memory_tool_enabled: bool,
    #[serde(default = "default_memory_tool_search_limit")]
    pub memory_tool_search_limit: u32,

    // PDF extraction (requires rag-pdf feature)
    #[serde(default = "default_true")]
    pub pdf_read_enabled: bool,
    #[serde(default = "default_pdf_read_max_chars")]
    pub pdf_read_max_chars: u32,

    // Image info tool
    #[serde(default = "default_true")]
    pub image_info_enabled: bool,

    /// Custom external tools defined in config.
    #[serde(default)]
    pub custom: Vec<CustomToolConfig>,
}

/// A custom tool that executes an external command.
#[derive(Debug, Deserialize, Clone)]
pub struct CustomToolConfig {
    pub name: String,
    pub description: String,
    pub command: String,
    #[serde(default = "default_custom_tool_timeout")]
    pub timeout_secs: u32,
}

fn default_custom_tool_timeout() -> u32 {
    120
}

impl Default for ToolsSection {
    fn default() -> Self {
        Self {
            shell_enabled: default_shell_enabled(),
            shell_allowed_commands: default_shell_allowed_commands(),
            shell_timeout_secs: default_shell_timeout_secs(),
            shell_permission_level: default_shell_permission_level(),
            shell_extra_allowed: vec![],
            shell_blocked_commands: default_shell_blocked_commands(),
            web_fetch_enabled: default_web_fetch_enabled(),
            web_fetch_timeout_secs: default_web_fetch_timeout_secs(),
            web_fetch_max_bytes: default_web_fetch_max_bytes(),
            file_enabled: default_file_enabled(),
            file_access_dir: default_file_access_dir(),
            web_blocked_domains: default_web_blocked_domains(),
            memory_tool_enabled: default_memory_tool_enabled(),
            memory_tool_search_limit: default_memory_tool_search_limit(),
            pdf_read_enabled: default_true(),
            pdf_read_max_chars: default_pdf_read_max_chars(),
            image_info_enabled: default_true(),
            custom: vec![],
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
    #[serde(default = "default_session_key_strategy")]
    pub session_key_strategy: String,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            history_limit: default_history_limit(),
            search_limit: default_search_limit(),
            session_key_strategy: default_session_key_strategy(),
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
    /// Fallback LLM profile name. Empty = no fallback.
    #[serde(default = "default_fallback_profile")]
    pub fallback_profile: String,
    /// Allow LLM to install packages autonomously. Default: false.
    #[serde(default)]
    pub auto_install_packages: bool,
    /// Install isolation: "venv" | "user" | "system". Default: "venv".
    #[serde(default = "default_install_scope")]
    pub install_scope: String,
    /// Virtual environment path (relative to workspace). Default: ".anqclaw/envs".
    #[serde(default = "default_venv_path")]
    pub venv_path: String,
    /// Max consecutive all-failed tool rounds before forcing stop. Default: 3.
    #[serde(default = "default_max_consecutive_tool_errors")]
    pub max_consecutive_tool_errors: u32,
    /// Extra binaries to probe at startup (appended to default list).
    #[serde(default)]
    pub probe_extra_binaries: Vec<String>,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            max_tool_rounds: default_max_tool_rounds(),
            system_prompt_file: default_system_prompt_file(),
            llm_profile: default_llm_profile(),
            fallback_profile: default_fallback_profile(),
            auto_install_packages: false,
            install_scope: default_install_scope(),
            venv_path: default_venv_path(),
            max_consecutive_tool_errors: default_max_consecutive_tool_errors(),
            probe_extra_binaries: Vec::new(),
        }
    }
}

// ─── Security ─────────────────────────────────────────────────────────────────

/// System directories that are ALWAYS blocked, regardless of config.
pub const HARDCODED_BLOCKED_DIRS: &[&str] = &[
    // Windows
    "C:\\Windows",
    "C:\\Program Files",
    "C:\\Program Files (x86)",
    "C:\\ProgramData",
    // Linux
    "/boot",
    "/sbin",
    "/usr/sbin",
    "/proc",
    "/sys",
    "/dev",
    "/etc/shadow",
    // macOS
    "/System",
    "/Library",
    "/private/var",
    // Sensitive user dirs (all platforms)
    ".ssh",
    ".gnupg",
    ".aws",
    ".config/gcloud",
    ".azure",
];

#[derive(Debug, Deserialize)]
pub struct SecuritySection {
    /// Directories where full permissions apply (shell full + file rw)
    #[serde(default = "default_trusted_dirs")]
    pub trusted_dirs: Vec<String>,
    /// Directories that are always blocked from any operation
    #[serde(default = "default_blocked_dirs")]
    pub blocked_dirs: Vec<String>,
    /// Automatically redact config secret values from LLM output
    #[serde(default = "default_auto_redact_secrets")]
    pub auto_redact_secrets: bool,
    /// Additional literal substrings to redact from LLM output (not regex)
    #[serde(default = "default_redact_patterns")]
    pub redact_patterns: Vec<String>,
}

impl Default for SecuritySection {
    fn default() -> Self {
        Self {
            trusted_dirs: default_trusted_dirs(),
            blocked_dirs: default_blocked_dirs(),
            auto_redact_secrets: default_auto_redact_secrets(),
            redact_patterns: default_redact_patterns(),
        }
    }
}

// ─── Limits ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LimitsSection {
    #[serde(default = "default_max_requests_per_minute")]
    pub max_requests_per_minute: u32,
    #[serde(default = "default_max_tokens_per_conversation")]
    pub max_tokens_per_conversation: u64,
    #[serde(default = "default_max_message_length")]
    pub max_message_length: u32,
}

impl Default for LimitsSection {
    fn default() -> Self {
        Self {
            max_requests_per_minute: default_max_requests_per_minute(),
            max_tokens_per_conversation: default_max_tokens_per_conversation(),
            max_message_length: default_max_message_length(),
        }
    }
}

// ─── Top-level config ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppConfig {
    pub app: AppSection,
    /// `None` if `[feishu]` / `[channel.feishu]` is omitted from config — Feishu channel won't start.
    pub feishu: Option<FeishuSection>,
    /// HTTP channel settings.
    pub http_channel: HttpChannelSection,
    /// Named LLM profiles. At least one ("default") is required.
    pub llm_profiles: HashMap<String, LlmSection>,
    /// Convenience accessor: the active LLM profile (determined by `agent.llm_profile`).
    /// This is a clone from `llm_profiles` — used by legacy code that expects a single `LlmSection`.
    pub llm: LlmSection,
    pub tools: ToolsSection,
    pub security: SecuritySection,
    pub limits: LimitsSection,
    pub memory: MemorySection,
    pub heartbeat: HeartbeatSection,
    pub agent: AgentSection,
    pub audit: AuditSection,
    pub skills: SkillsSection,
    pub scheduler: SchedulerSection,
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
        // [channel.feishu] takes precedence over legacy [feishu]
        let http_channel = match raw.channel.as_ref().and_then(|c| c.http.clone()) {
            Some(raw_http) => HttpChannelSection {
                enabled: raw_http.enabled,
                bind: raw_http.bind,
                bearer_token: SecretString::new(raw_http.bearer_token.into()),
            },
            None => HttpChannelSection::default(),
        };

        let raw_feishu = raw.channel.and_then(|c| c.feishu).or(raw.feishu);
        let feishu = match raw_feishu {
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
            let api_key_str = resolve_env_optional(&raw_p.api_key);
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
                    max_retries: raw_p.max_retries,
                    retry_delay_ms: raw_p.retry_delay_ms,
                },
            );
        }

        // Determine active profile
        let active_profile_name = &raw.agent.llm_profile;
        let active_profile = llm_profiles.get(active_profile_name).ok_or_else(|| {
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
            max_retries: active_profile.max_retries,
            retry_delay_ms: active_profile.retry_delay_ms,
        };

        Ok(AppConfig {
            app: raw.app,
            feishu,
            http_channel,
            llm_profiles,
            llm,
            tools: raw.tools,
            security: raw.security,
            limits: raw.limits,
            memory: raw.memory,
            heartbeat: raw.heartbeat,
            agent: raw.agent,
            audit: raw.audit,
            skills: raw.skills,
            scheduler: raw.scheduler,
        })
    }
}
