//! @file
//! @author lijianqiao
//! @since 2026-03-31
//! @brief 提供配置结构体使用的默认值函数。

// ─── Default value helpers ────────────────────────────────────────────────────
//
// Each function is referenced by `#[serde(default = "...")]` on config structs.

pub(crate) fn default_app_name() -> String {
    "anqclaw".to_string()
}
pub(crate) fn default_workspace() -> String {
    "workspace".to_string()
}
pub(crate) fn default_log_level() -> String {
    "info".to_string()
}
pub(crate) fn default_allow_from() -> Vec<String> {
    vec![]
}
pub(crate) fn default_llm_provider() -> String {
    "anthropic".to_string()
}
pub(crate) fn default_llm_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
pub(crate) fn default_base_url() -> String {
    String::new()
}
pub(crate) fn default_max_tokens() -> u32 {
    4096
}
pub(crate) fn default_temperature() -> f32 {
    0.7
}
pub(crate) fn default_supports_tools() -> bool {
    true
}
pub(crate) fn default_db_path() -> String {
    "data/memory.db".to_string()
}
pub(crate) fn default_history_limit() -> u32 {
    20
}
pub(crate) fn default_search_limit() -> u32 {
    5
}
pub(crate) fn default_heartbeat_enabled() -> bool {
    false
}
pub(crate) fn default_interval_minutes() -> u32 {
    30
}
pub(crate) fn default_notify_channel() -> String {
    "feishu".to_string()
}
pub(crate) fn default_notify_chat_id() -> String {
    String::new()
}
pub(crate) fn default_shell_enabled() -> bool {
    true
}
pub(crate) fn default_shell_allowed_commands() -> Vec<String> {
    vec![
        "ls".to_string(),
        "cat".to_string(),
        "grep".to_string(),
        "find".to_string(),
        "date".to_string(),
        "curl".to_string(),
    ]
}
pub(crate) fn default_shell_timeout_secs() -> u32 {
    30
}
pub(crate) fn default_web_fetch_enabled() -> bool {
    true
}
pub(crate) fn default_web_fetch_timeout_secs() -> u32 {
    10
}
pub(crate) fn default_web_fetch_max_bytes() -> u64 {
    102400
}
pub(crate) fn default_file_enabled() -> bool {
    true
}
pub(crate) fn default_file_access_dir() -> String {
    "workspace".to_string()
}
pub(crate) fn default_web_blocked_domains() -> Vec<String> {
    vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "0.0.0.0".to_string(),
        "169.254.169.254".to_string(), // Cloud metadata SSRF
        "[::1]".to_string(),
    ]
}
pub(crate) fn default_memory_tool_enabled() -> bool {
    true
}
pub(crate) fn default_memory_tool_search_limit() -> u32 {
    5
}
pub(crate) fn default_pdf_read_max_chars() -> u32 {
    50000
}
pub(crate) fn default_shell_permission_level() -> String {
    "supervised".to_string()
}
pub(crate) fn default_shell_blocked_commands() -> Vec<String> {
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
pub(crate) fn default_trusted_dirs() -> Vec<String> {
    vec![]
}
pub(crate) fn default_blocked_dirs() -> Vec<String> {
    vec![]
}
pub(crate) fn default_auto_redact_secrets() -> bool {
    true
}
pub(crate) fn default_redact_patterns() -> Vec<String> {
    vec![]
}
pub(crate) fn default_max_requests_per_minute() -> u32 {
    20
}
pub(crate) fn default_max_tokens_per_conversation() -> u64 {
    50000
}
pub(crate) fn default_max_message_length() -> u32 {
    10000
}
pub(crate) fn default_max_tool_rounds() -> u32 {
    10
}
pub(crate) fn default_system_prompt_file() -> String {
    String::new()
}
pub(crate) fn default_llm_profile() -> String {
    "default".to_string()
}
pub(crate) fn default_max_retries() -> u32 {
    2
}
pub(crate) fn default_retry_delay_ms() -> u64 {
    1000
}
pub(crate) fn default_fallback_profile() -> String {
    String::new()
}
pub(crate) fn default_install_scope() -> String {
    "venv".to_string()
}
pub(crate) fn default_venv_path() -> String {
    "workspace/.venv".to_string()
}
pub(crate) fn default_managed_python_version() -> String {
    "3.12".to_string()
}
pub(crate) fn default_max_consecutive_tool_errors() -> u32 {
    3
}
pub(crate) fn default_audit_enabled() -> bool {
    false
}
pub(crate) fn default_audit_log_file() -> String {
    "logs/audit.jsonl".to_string()
}
pub(crate) fn default_log_tool_calls() -> bool {
    true
}
pub(crate) fn default_log_shell_commands() -> bool {
    true
}
pub(crate) fn default_log_file_writes() -> bool {
    true
}
pub(crate) fn default_log_llm_calls() -> bool {
    false
}
pub(crate) fn default_skills_enabled() -> bool {
    true
}
pub(crate) fn default_skills_dir() -> String {
    "skills".to_string()
}
pub(crate) fn default_max_active_skills() -> u32 {
    3
}
pub(crate) fn default_max_skills_in_prompt() -> u32 {
    32
}
pub(crate) fn default_max_skill_prompt_chars() -> u32 {
    12_000
}
pub(crate) fn default_max_skill_file_bytes() -> u64 {
    256 * 1024
}
pub(crate) fn default_session_key_strategy() -> String {
    "chat".to_string()
}
pub(crate) fn default_scheduler_enabled() -> bool {
    false
}
pub(crate) fn default_http_bind() -> String {
    "127.0.0.1:3000".to_string()
}
pub(crate) fn default_true() -> bool {
    true
}
pub(crate) fn default_custom_tool_timeout() -> u32 {
    120
}
