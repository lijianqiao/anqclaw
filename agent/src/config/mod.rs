mod defaults;
mod sections;

pub use sections::*;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use defaults::*;

// ─── Raw deserialization structs (secrets as plain String) ────────────────────

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
    let table = llm_value
        .as_table()
        .context("[llm] must be a TOML table / [llm] 必须是 TOML 表")?;

    // Detect: if it has a "provider" or "model" or "api_key" key at the top level,
    // treat as legacy single profile → wrap as { "default": ... }
    let is_legacy = table.contains_key("provider")
        || table.contains_key("model")
        || table.contains_key("api_key");

    if is_legacy {
        let profile: RawLlmProfile = toml::Value::Table(table.clone())
            .try_into()
            .context("parse [llm] as flat profile / 解析 [llm] 为扁平配置失败")?;
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
                .with_context(|| format!("parse [llm.{name}] / 解析 [llm.{name}] 失败"))?;
            map.insert(name.clone(), profile);
        }
        if map.is_empty() {
            anyhow::bail!(
                "[llm] section is empty — at least one LLM profile is required / [llm] 段为空 - 至少需要一个 LLM 配置"
            );
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

fn resolve_env_ref(value: &str) -> Option<&str> {
    value.strip_prefix("${").and_then(|s| s.strip_suffix('}'))
}

/// If `value` looks like `${VAR_NAME}`, read the environment variable `VAR_NAME`.
/// Otherwise return the value unchanged.
/// Returns an error if the referenced env var is not set.
fn resolve_env(value: &str, field_name: &str) -> Result<String> {
    if let Some(inner) = resolve_env_ref(value) {
        std::env::var(inner).with_context(|| {
            format!(
                "Config field `{}` references env var `{}` which is not set / 配置字段 `{}` 引用的环境变量 `{}` 未设置",
                field_name, inner, field_name, inner
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
    if let Some(inner) = resolve_env_ref(value) {
        std::env::var(inner).unwrap_or_default()
    } else {
        value.to_string()
    }
}

// ─── AppConfig::load ──────────────────────────────────────────────────────────

impl AppConfig {
    /// Load configuration from a TOML file.
    pub fn load(path: &str) -> Result<Self> {
        let raw_text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "Cannot read config file: {} / 无法读取配置文件: {}",
                path, path
            )
        })?;

        Self::load_from_str(&raw_text)
    }

    /// Parse configuration from a TOML string (useful for tests).
    pub fn load_from_str(toml_text: &str) -> Result<Self> {
        let raw: RawAppConfig = toml::from_str(toml_text)
            .context("Failed to parse config TOML / 解析配置 TOML 失败")?;

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
                "agent.llm_profile = \"{}\" but no [llm.{}] profile found. Available: {:?} / agent.llm_profile = \"{}\" 但未找到 [llm.{}] 配置。可用: {:?}",
                active_profile_name,
                active_profile_name,
                llm_profiles.keys().collect::<Vec<_>>(),
                active_profile_name,
                active_profile_name,
                llm_profiles.keys().collect::<Vec<_>>()
            )
        })?;

        // Clone active profile into the convenience `llm` field.
        // Use expose_secret() to rebuild SecretString from the already-resolved value,
        // avoiding a second env-var resolution that could yield a different key.
        let llm = LlmSection {
            provider: active_profile.provider.clone(),
            model: active_profile.model.clone(),
            api_key: SecretString::new(active_profile.api_key.expose_secret().to_owned().into()),
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

    /// Resolve all user-facing path fields against the anqclaw home directory.
    pub fn resolve_paths_against(&mut self, home: &Path) {
        self.app.workspace = crate::paths::resolve_path(home, &self.app.workspace)
            .to_string_lossy()
            .into_owned();
        self.memory.db_path = crate::paths::resolve_path(home, &self.memory.db_path)
            .to_string_lossy()
            .into_owned();
        self.tools.file_access_dir = crate::paths::resolve_path(home, &self.tools.file_access_dir)
            .to_string_lossy()
            .into_owned();
        self.agent.venv_path = crate::paths::resolve_path(home, &self.agent.venv_path)
            .to_string_lossy()
            .into_owned();
        if !self.app.log_file.is_empty() {
            self.app.log_file = crate::paths::resolve_path(home, &self.app.log_file)
                .to_string_lossy()
                .into_owned();
        }
        if !self.audit.log_file.is_empty() {
            self.audit.log_file = crate::paths::resolve_path(home, &self.audit.log_file)
                .to_string_lossy()
                .into_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISSING_ENV_VAR: &str = "ANQCLAW_TEST_MISSING_ENV_VAR_8F82E6A9";

    #[test]
    fn resolve_env_ref_extracts_wrapped_var_name() {
        assert_eq!(resolve_env_ref("${TOKEN}"), Some("TOKEN"));
        assert_eq!(resolve_env_ref("TOKEN"), None);
        assert_eq!(resolve_env_ref("${TOKEN"), None);
        assert_eq!(resolve_env_ref("$TOKEN}"), None);
    }

    #[test]
    fn resolve_env_preserves_literal_values() {
        assert_eq!(resolve_env("literal", "field").unwrap(), "literal");
    }

    #[test]
    fn resolve_env_reports_missing_required_reference() {
        let error = resolve_env(&format!("${{{MISSING_ENV_VAR}}}"), "feishu.app_secret")
            .expect_err("missing env reference should fail / 缺失环境变量引用应失败");
        let message = error.to_string();
        assert!(message.contains("feishu.app_secret"));
        assert!(message.contains(MISSING_ENV_VAR));
    }

    #[test]
    fn resolve_env_optional_returns_empty_for_missing_reference() {
        assert_eq!(
            resolve_env_optional(&format!("${{{MISSING_ENV_VAR}}}")),
            String::new()
        );
    }
}
