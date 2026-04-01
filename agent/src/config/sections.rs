//! @file
//! @author lijianqiao
//! @since 2026-03-31
//! @brief 定义应用配置各分段的数据结构。

use secrecy::SecretString;
use serde::Deserialize;

use super::defaults::*;

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
    #[serde(default = "default_max_skills_in_prompt")]
    pub max_skills_in_prompt: u32,
    #[serde(default = "default_max_skill_prompt_chars")]
    pub max_skill_prompt_chars: u32,
    #[serde(default = "default_max_skill_file_bytes")]
    pub max_skill_file_bytes: u64,
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self {
            enabled: default_skills_enabled(),
            skills_dir: default_skills_dir(),
            max_active_skills: default_max_active_skills(),
            max_skills_in_prompt: default_max_skills_in_prompt(),
            max_skill_prompt_chars: default_max_skill_prompt_chars(),
            max_skill_file_bytes: default_max_skill_file_bytes(),
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
    #[serde(default)]
    pub executable: String,
    #[serde(default)]
    pub base_args: Vec<String>,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub command: String,
    #[serde(default = "default_custom_tool_timeout")]
    pub timeout_secs: u32,
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
    /// Allow LLM to install packages autonomously. When `install_scope = "venv"`,
    /// shell_exec may prepare a workspace-local managed runtime and install
    /// packages automatically. This does not require a preinstalled system
    /// Python, but it does require a locally available `uv`. Default: false.
    #[serde(default)]
    pub auto_install_packages: bool,
    /// Install isolation: "venv" | "user" | "system". Default: "venv".
    #[serde(default = "default_install_scope")]
    pub install_scope: String,
    /// Virtual environment path (relative to ~/.anqclaw/). Created on first
    /// Python-oriented task when local `uv` is available; the requested Python
    /// version may be installed on demand into this managed environment.
    /// Default: "workspace/.venv".
    #[serde(default = "default_venv_path")]
    pub venv_path: String,
    /// Managed Python version requested when bootstrapping the workspace-local
    /// isolated runtime with `uv`.
    #[serde(default = "default_managed_python_version")]
    pub managed_python_version: String,
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
            managed_python_version: default_managed_python_version(),
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
