//! OpenAI-compatible LLM client.
//!
//! Covers: OpenAI, DeepSeek, Qwen, MiMo, Gemini (via OpenAI-compat endpoint), and
//! any other provider that speaks the `/v1/chat/completions` protocol.
//!
//! Differences between providers are handled entirely by `base_url` + `api_key` +
//! `model` in config — no per-provider special-casing needed.

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
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl OpenAiCompatClient {
    pub fn new(config: &LlmSection) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("build reqwest client");

        Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key.expose_secret().to_string(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
        }
    }

    /// Performs the actual HTTP request with retry logic.
    async fn do_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let req_messages: Vec<OaiMessage> = messages.iter().map(to_oai_message).collect();

        let req_tools: Vec<OaiTool> = tools.iter().map(to_oai_tool).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": req_messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });

        if !req_tools.is_empty() {
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

            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

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
