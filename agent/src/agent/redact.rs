//! Output redaction — removes sensitive information from LLM responses.
//!
//! Prevents accidental leakage of API keys, tokens, and other secrets.

use crate::config::AppConfig;
use secrecy::ExposeSecret;

/// Built-in patterns that look like common API keys/tokens.
/// These are checked as substrings or simple prefix patterns.
const BUILTIN_PATTERNS: &[&str] = &[
    "sk-ant-",     // Anthropic
    "sk-proj-",    // OpenAI project keys
    "ghp_",        // GitHub personal access tokens
    "gho_",        // GitHub OAuth tokens
    "github_pat_", // GitHub fine-grained PATs
    "glpat-",      // GitLab PATs
    "xoxb-",       // Slack bot tokens
    "xoxp-",       // Slack user tokens
];

/// Build a list of secret values to redact from the config.
pub fn collect_secrets(config: &AppConfig) -> Vec<String> {
    let mut secrets = Vec::new();

    // Primary LLM API key (may differ from profiles if re-resolved)
    let primary_key = config.llm.api_key.expose_secret().to_string();
    if primary_key.len() >= 8 {
        secrets.push(primary_key);
    }

    // All LLM profile API keys
    for profile in config.llm_profiles.values() {
        let key = profile.api_key.expose_secret().to_string();
        if key.len() >= 8 && !secrets.contains(&key) {
            secrets.push(key);
        }
    }

    // Feishu app secret
    if let Some(ref feishu) = config.feishu {
        let secret = feishu.app_secret.expose_secret().to_string();
        if secret.len() >= 8 {
            secrets.push(secret);
        }
    }

    // HTTP channel bearer token
    let bearer = config.http_channel.bearer_token.expose_secret().to_string();
    if bearer.len() >= 8 {
        secrets.push(bearer);
    }

    secrets
}

/// Redact sensitive information from text.
///
/// Returns the redacted text with secrets replaced by `[REDACTED]`.
pub fn redact_output(text: &str, secrets: &[String], extra_patterns: &[String]) -> String {
    let mut result = text.to_string();

    // 1. Redact known secret values from config
    for secret in secrets {
        if !secret.is_empty() && result.contains(secret.as_str()) {
            result = result.replace(secret.as_str(), "[REDACTED]");
        }
    }

    // 2. Redact built-in patterns (prefix-based token detection)
    for pattern in BUILTIN_PATTERNS {
        // Find tokens that start with this prefix and redact the whole token
        // A "token" here is a contiguous non-whitespace string
        let mut redacted = String::new();
        let mut remaining = result.as_str();
        while let Some(pos) = remaining.find(pattern) {
            redacted.push_str(&remaining[..pos]);
            // Find the end of the token (next whitespace or end of string)
            let token_start = pos;
            let after_prefix = &remaining[pos..];
            let token_end = after_prefix
                .find(|c: char| {
                    c.is_whitespace()
                        || c == '"'
                        || c == '\''
                        || c == ','
                        || c == '}'
                        || c == ']'
                })
                .unwrap_or(after_prefix.len());
            redacted.push_str("[REDACTED]");
            remaining = &remaining[token_start + token_end..];
        }
        redacted.push_str(remaining);
        result = redacted;
    }

    // 3. Redact extra user-defined patterns (treated as simple substring matches)
    // Note: For simplicity, we treat these as literal substrings, not regex.
    // If the user wants regex, we can add regex crate later.
    for pattern in extra_patterns {
        if !pattern.is_empty() && result.contains(pattern.as_str()) {
            result = result.replace(pattern.as_str(), "[REDACTED]");
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_api_key() {
        let secrets = vec!["***REMOVED***".to_string()];
        let text = "The API key is ***REMOVED***, don't share it.";
        let result = redact_output(text, &secrets, &[]);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("***REMOVED***"));
    }

    #[test]
    fn test_redact_builtin_patterns() {
        let text = "Found token: sk-ant-abc123xyz456 in the config";
        let result = redact_output(text, &[], &[]);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("sk-ant-abc123xyz456"));
    }

    #[test]
    fn test_redact_github_pat() {
        let text = "Use ghp_1234567890abcdefghijklmnopqrstuvwxyz for auth";
        let result = redact_output(text, &[], &[]);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn test_no_redact_when_no_secrets() {
        let text = "Hello, this is a normal message.";
        let result = redact_output(text, &[], &[]);
        assert_eq!(result, text);
    }

    #[test]
    fn test_redact_extra_patterns() {
        let text = "My password is hunter2 and my name is John";
        let result = redact_output(text, &[], &["hunter2".to_string()]);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("hunter2"));
    }
}
