//! OpenAI-compatible LLM client.
//!
//! Covers: OpenAI, DeepSeek, Qwen, MiMo, Gemini, Ollama, and any other provider
//! that speaks the `/v1/chat/completions` protocol.
//!
//! Provider quirks are handled by config fields: `base_url`, `api_key` (optional),
//! `supports_tools`, etc. — no per-provider special-casing in code.

use anyhow::{Context, Result};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::config::LlmSection;
use crate::types::{ChatMessage, LlmResponse, Role, ToolCall, ToolDefinition};

use super::LlmClient;

// ─── Client ──────────────────────────────────────────────────────────────────

pub struct OpenAiCompatClient {
    http: reqwest::Client,
    /// Full URL to the chat completions endpoint (computed once at construction).
    endpoint: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    supports_tools: bool,
}

impl OpenAiCompatClient {
    pub fn new(config: &LlmSection) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("build reqwest client");

        let endpoint = build_endpoint(&config.base_url);

        Self {
            http,
            endpoint,
            api_key: config.api_key.expose_secret().to_string(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            supports_tools: config.supports_tools,
        }
    }

    /// Performs the actual HTTP request with retry logic.
    async fn do_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let req_messages: Vec<OaiMessage> = messages.iter().map(to_oai_message).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": req_messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });

        // Only include tools if the model supports them AND tools are provided
        if self.supports_tools && !tools.is_empty() {
            let req_tools: Vec<OaiTool> = tools.iter().map(to_oai_tool).collect();
            body["tools"] = serde_json::to_value(&req_tools)?;
        }

        // Retry up to 3 times on 429 / 5xx
        let mut last_err = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let backoff = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                tracing::warn!(attempt, ?backoff, "retrying OpenAI-compat request");
                tokio::time::sleep(backoff).await;
            }

            let mut req = self
                .http
                .post(&self.endpoint)
                .header("Content-Type", "application/json");

            // Only add Authorization header if api_key is non-empty (Ollama doesn't need it)
            if !self.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", self.api_key));
            }

            let resp = req.json(&body).send().await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let oai_resp: OaiResponse = r
                        .json()
                        .await
                        .context("deserialise OpenAI-compat response")?;
                    return parse_oai_response(oai_resp);
                }
                Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    tracing::warn!(%status, body = %text, "retryable error from OpenAI-compat");
                    last_err = Some(anyhow::anyhow!("HTTP {status}: {text}"));
                }
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    anyhow::bail!("OpenAI-compat non-retryable error HTTP {status}: {text}");
                }
                Err(e) => {
                    last_err = Some(e.into());
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("OpenAI-compat request failed after retries")))
    }
}

impl LlmClient for OpenAiCompatClient {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
        Box::pin(self.do_chat(messages, tools))
    }
}

// ─── Smart URL construction ─────────────────────────────────────────────────

/// Builds the full endpoint URL from a base URL, handling these common cases:
///
/// | base_url                                               | result                                                   |
/// |--------------------------------------------------------|----------------------------------------------------------|
/// | `https://api.openai.com`                               | `https://api.openai.com/v1/chat/completions`             |
/// | `https://api.openai.com/v1`                            | `https://api.openai.com/v1/chat/completions`             |
/// | `https://api.openai.com/v1/`                           | `https://api.openai.com/v1/chat/completions`             |
/// | `https://proxy.example.com/v1/chat/completions`        | `https://proxy.example.com/v1/chat/completions` (as-is)  |
/// | `https://generativelanguage.googleapis.com/v1beta/openai` | `…/v1beta/openai/chat/completions`                    |
/// | `http://localhost:11434`                               | `http://localhost:11434/v1/chat/completions`             |
fn build_endpoint(base_url: &str) -> String {
    let url = base_url.trim_end_matches('/');

    if url.is_empty() {
        // Default to OpenAI
        return "https://api.openai.com/v1/chat/completions".to_string();
    }

    // Already has the full path — use as-is
    if url.ends_with("/chat/completions") {
        return url.to_string();
    }

    // Ends with /v1 — just append /chat/completions
    if url.ends_with("/v1") {
        return format!("{url}/chat/completions");
    }

    // Otherwise append /v1/chat/completions
    format!("{url}/v1/chat/completions")
}

// ─── OpenAI wire types (private) ─────────────────────────────────────────────

#[derive(Serialize)]
struct OaiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCallRequest>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct OaiToolCallRequest {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OaiFunction,
}

#[derive(Serialize)]
struct OaiFunction {
    name: String,
    arguments: String, // JSON string
}

#[derive(Serialize)]
struct OaiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OaiToolFunction,
}

#[derive(Serialize)]
struct OaiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ── Response types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OaiResponse {
    choices: Vec<OaiChoice>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiResponseMessage,
}

#[derive(Deserialize)]
struct OaiResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OaiToolCallResponse>,
}

#[derive(Deserialize)]
struct OaiToolCallResponse {
    id: String,
    function: OaiFunctionResponse,
}

#[derive(Deserialize)]
struct OaiFunctionResponse {
    name: String,
    arguments: String,
}

// ─── Mapping helpers ─────────────────────────────────────────────────────────

fn to_oai_message(msg: &ChatMessage) -> OaiMessage {
    match msg.role {
        Role::System => OaiMessage {
            role: "system".into(),
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: None,
        },
        Role::User => OaiMessage {
            role: "user".into(),
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: None,
        },
        Role::Assistant => {
            let tool_calls = msg.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|c| OaiToolCallRequest {
                        id: c.id.clone(),
                        call_type: "function".into(),
                        function: OaiFunction {
                            name: c.name.clone(),
                            arguments: c.arguments.to_string(),
                        },
                    })
                    .collect()
            });

            OaiMessage {
                role: "assistant".into(),
                content: if msg.content.is_empty() {
                    None
                } else {
                    Some(msg.content.clone())
                },
                tool_calls,
                tool_call_id: None,
            }
        }
        Role::Tool => OaiMessage {
            role: "tool".into(),
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: msg.tool_call_id.clone(),
        },
    }
}

fn to_oai_tool(def: &ToolDefinition) -> OaiTool {
    OaiTool {
        tool_type: "function".into(),
        function: OaiToolFunction {
            name: def.name.clone(),
            description: def.description.clone(),
            parameters: def.parameters.clone(),
        },
    }
}

fn parse_oai_response(resp: OaiResponse) -> Result<LlmResponse> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .context("OpenAI-compat response has no choices")?;

    let text = choice.message.content;

    let tool_calls: Vec<ToolCall> = choice
        .message
        .tool_calls
        .into_iter()
        .map(|tc| {
            let arguments: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
            ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments,
            }
        })
        .collect();

    Ok(LlmResponse { text, tool_calls })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_endpoint_plain_domain() {
        assert_eq!(
            build_endpoint("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_with_v1() {
        assert_eq!(
            build_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_with_v1_trailing_slash() {
        assert_eq!(
            build_endpoint("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_full_path() {
        assert_eq!(
            build_endpoint("https://proxy.example.com/v1/chat/completions"),
            "https://proxy.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_gemini() {
        assert_eq!(
            build_endpoint("https://generativelanguage.googleapis.com/v1beta/openai"),
            "https://generativelanguage.googleapis.com/v1beta/openai/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_ollama() {
        assert_eq!(
            build_endpoint("http://localhost:11434"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_deepseek() {
        assert_eq!(
            build_endpoint("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_endpoint_empty_defaults_to_openai() {
        assert_eq!(
            build_endpoint(""),
            "https://api.openai.com/v1/chat/completions"
        );
    }
}
