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
use crate::types::{ChatMessage, LlmResponse, Role, StreamEvent, ToolCall, ToolDefinition};

use super::{LlmClient, StreamToolCallAccumulator, finalize_stream_response};

// ─── Client ──────────────────────────────────────────────────────────────────

pub struct OpenAiCompatClient {
    http: reqwest::Client,
    /// Full URL to the chat completions endpoint (computed once at construction).
    endpoint: String,
    api_key: secrecy::SecretString,
    model: String,
    max_tokens: u32,
    temperature: f32,
    supports_tools: bool,
    /// Extra headers sent with every request (e.g. OpenRouter's HTTP-Referer).
    extra_headers: Vec<(String, String)>,
}

impl OpenAiCompatClient {
    pub fn new(config: &LlmSection) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("build reqwest client / 构建 reqwest 客户端失败")?;

        let endpoint = build_endpoint(&config.base_url);
        let extra_headers = provider_extra_headers(&config.provider);

        Ok(Self {
            http,
            endpoint,
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            supports_tools: config.supports_tools,
            extra_headers,
        })
    }

    fn apply_request_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req.header("Content-Type", "application/json");

        let key = self.api_key.expose_secret();
        if !key.is_empty() {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        for (name, value) in &self.extra_headers {
            req = req.header(name.as_str(), value.as_str());
        }

        req
    }

    /// Performs the actual HTTP request (retry is handled by outer RetryLlmClient).
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

        // Retry is handled by the outer RetryLlmClient — no internal retry here.
        let req = self.apply_request_headers(self.http.post(&self.endpoint));

        let resp = req
            .json(&body)
            .send()
            .await
            .context("OpenAI-compat HTTP request failed / OpenAI 兼容 HTTP 请求失败")?;

        if resp.status().is_success() {
            let oai_resp: OaiResponse = resp
                .json()
                .await
                .context("deserialise OpenAI-compat response / 反序列化 OpenAI 兼容响应失败")?;
            return parse_oai_response(oai_resp);
        }

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI-compat HTTP {status}: {text}");
    }

    /// Streaming version — sends SSE request, returns a channel of StreamEvents.
    async fn do_chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let req_messages: Vec<OaiMessage> = messages.iter().map(to_oai_message).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": req_messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "stream": true,
        });

        if self.supports_tools && !tools.is_empty() {
            let req_tools: Vec<OaiTool> = tools.iter().map(to_oai_tool).collect();
            body["tools"] = serde_json::to_value(&req_tools)?;
        }

        let req = self.apply_request_headers(self.http.post(&self.endpoint));

        let response = req.json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "OpenAI-compat streaming error HTTP {status}: {text} / OpenAI 兼容流式错误 HTTP {status}: {text}"
            );
        }

        let (tx, rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut full_text = String::new();
            let mut tc_acc = StreamToolCallAccumulator::new();
            let mut response = response;
            let mut done = false;
            const MAX_BUFFER_SIZE: usize = 512 * 1024; // 512 KB safety limit

            while !done {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                        if buffer.len() > MAX_BUFFER_SIZE {
                            tracing::warn!(
                                "OpenAI SSE buffer exceeded limit, truncating / OpenAI SSE 缓冲区超出限制，正在截断"
                            );
                            break;
                        }
                    }
                    _ => break,
                }

                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim_end().to_string();
                    // Efficient: drain processed bytes instead of copying entire remainder
                    buffer.drain(..=pos);

                    let data = match line.strip_prefix("data: ") {
                        Some(d) => d,
                        None => continue,
                    };

                    if data == "[DONE]" {
                        done = true;
                        break;
                    }

                    let Ok(chunk) = serde_json::from_str::<OaiStreamChunk>(data) else {
                        continue;
                    };

                    if let Some(choice) = chunk.choices.first() {
                        if let Some(ref content) = choice.delta.content
                            && !content.is_empty()
                        {
                            full_text.push_str(content);
                            let _ = tx.send(StreamEvent::Delta(content.clone())).await;
                        }
                        if let Some(ref tcs) = choice.delta.tool_calls {
                            for tc in tcs {
                                let entry = tc_acc.entry(tc.index).or_default();
                                if let Some(ref id) = tc.id {
                                    entry.0.clone_from(id);
                                }
                                if let Some(ref f) = tc.function {
                                    if let Some(ref name) = f.name {
                                        entry.1.clone_from(name);
                                    }
                                    if let Some(ref args) = f.arguments {
                                        entry.2.push_str(args);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let resp = finalize_stream_response(full_text, tc_acc);
            let _ = tx.send(StreamEvent::Done(resp)).await;
        });

        Ok(rx)
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

    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<tokio::sync::mpsc::Receiver<StreamEvent>>> + Send + 'a>>
    {
        Box::pin(self.do_chat_stream(messages, tools))
    }
}

fn provider_extra_headers(provider: &str) -> Vec<(String, String)> {
    if provider == "openrouter" {
        vec![
            (
                "HTTP-Referer".to_string(),
                "https://github.com/anqclaw".to_string(),
            ),
            ("X-Title".to_string(), "anqclaw".to_string()),
        ]
    } else {
        Vec::new()
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
    content: Option<OaiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCallRequest>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// OpenAI content can be a simple string or an array of content parts (for vision).
#[derive(Serialize)]
#[serde(untagged)]
enum OaiContent {
    Text(String),
    Parts(Vec<OaiContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum OaiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OaiImageUrl },
}

#[derive(Serialize)]
struct OaiImageUrl {
    url: String, // "data:<media_type>;base64,<data>"
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
            content: Some(OaiContent::Text(msg.content.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        Role::User => {
            // Check if this user message has image attachments
            if let Some(ref images) = msg.images {
                let mut parts = Vec::new();
                for img in images {
                    parts.push(OaiContentPart::ImageUrl {
                        image_url: OaiImageUrl {
                            url: format!("data:{};base64,{}", img.media_type, img.data),
                        },
                    });
                }
                if !msg.content.is_empty() {
                    parts.push(OaiContentPart::Text {
                        text: msg.content.clone(),
                    });
                }
                OaiMessage {
                    role: "user".into(),
                    content: Some(OaiContent::Parts(parts)),
                    tool_calls: None,
                    tool_call_id: None,
                }
            } else {
                OaiMessage {
                    role: "user".into(),
                    content: Some(OaiContent::Text(msg.content.clone())),
                    tool_calls: None,
                    tool_call_id: None,
                }
            }
        }
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
                    Some(OaiContent::Text(msg.content.clone()))
                },
                tool_calls,
                tool_call_id: None,
            }
        }
        Role::Tool => OaiMessage {
            role: "tool".into(),
            content: Some(OaiContent::Text(msg.content.clone())),
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
        .context("OpenAI-compat response has no choices / OpenAI 兼容响应没有选项")?;

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

// ─── Streaming response types ────────────────────────────────────────────────

#[derive(Deserialize)]
struct OaiStreamChunk {
    choices: Vec<OaiStreamChoice>,
}

#[derive(Deserialize)]
struct OaiStreamChoice {
    delta: OaiStreamDelta,
}

#[derive(Deserialize)]
struct OaiStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OaiStreamToolCall>>,
}

#[derive(Deserialize)]
struct OaiStreamToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OaiStreamFunction>,
}

#[derive(Deserialize)]
struct OaiStreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{StreamToolCallAccumulator, finalize_stream_response};

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

    #[test]
    fn test_provider_extra_headers_openrouter() {
        assert_eq!(
            provider_extra_headers("openrouter"),
            vec![
                (
                    "HTTP-Referer".to_string(),
                    "https://github.com/anqclaw".to_string()
                ),
                ("X-Title".to_string(), "anqclaw".to_string()),
            ]
        );
    }

    #[test]
    fn test_provider_extra_headers_other_provider_empty() {
        assert!(provider_extra_headers("openai").is_empty());
    }

    #[test]
    fn test_finalize_stream_response_orders_tool_calls_and_nulls_invalid_json() {
        let mut tc_acc = StreamToolCallAccumulator::new();
        tc_acc.insert(2, ("call_2".into(), "second".into(), "{invalid".into()));
        tc_acc.insert(
            0,
            ("call_0".into(), "first".into(), r#"{"ok":true}"#.into()),
        );

        let response = finalize_stream_response("hello".into(), tc_acc);

        assert_eq!(response.text.as_deref(), Some("hello"));
        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].id, "call_0");
        assert_eq!(response.tool_calls[0].name, "first");
        assert_eq!(
            response.tool_calls[0].arguments,
            serde_json::json!({"ok": true})
        );
        assert_eq!(response.tool_calls[1].id, "call_2");
        assert_eq!(response.tool_calls[1].name, "second");
        assert_eq!(response.tool_calls[1].arguments, serde_json::Value::Null);
    }
}
