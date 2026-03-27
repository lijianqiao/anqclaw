//! Anthropic Messages API client (Claude).
//!
//! Key differences from OpenAI format:
//! - System prompt goes in a top-level `system` field, not in the messages array.
//! - Tool calls use `tool_use` / `tool_result` content blocks (not the OpenAI
//!   `tool_calls` array on the message object).
//! - Header auth uses `x-api-key` instead of `Authorization: Bearer`.
//! - Extra retryable status: 529 (Overloaded).

use anyhow::{Context, Result};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::config::LlmSection;
use crate::types::{ChatMessage, LlmResponse, Role, StreamEvent, ToolCall, ToolDefinition};

use super::LlmClient;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ─── Client ──────────────────────────────────────────────────────────────────

pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: secrecy::SecretString,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl AnthropicClient {
    pub fn new(config: &LlmSection) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("build reqwest client")?;

        Ok(Self {
            http,
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
        })
    }

    async fn do_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        // 1. Extract system prompt (all System messages joined)
        let system_text: String = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        // 2. Convert non-system messages to Anthropic format
        let ant_messages: Vec<AntMessage<'_>> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|msg| to_ant_message(msg))
            .collect();

        // 3. Convert tool definitions
        let ant_tools: Vec<AntTool<'_>> = tools.iter().map(|tool| to_ant_tool(tool)).collect();

        // 4. Build request body
        let mut body = AntRequest {
            model: self.model.as_str(),
            max_tokens: self.max_tokens,
            temperature: Some(self.temperature),
            system: if system_text.is_empty() {
                None
            } else {
                Some(system_text)
            },
            messages: ant_messages,
            tools: if ant_tools.is_empty() {
                None
            } else {
                Some(ant_tools)
            },
            stream: None,
        };

        // Anthropic requires `messages` to be non-empty and start with a user message.
        // If the history is empty (e.g. heartbeat), inject a placeholder.
        if body.messages.is_empty() {
            body.messages.push(AntMessage {
                role: "user",
                content: AntContent::Text(Cow::Borrowed("(no user input)")),
            });
        }

        // 5. HTTP with retry (429, 500, 529)
        let mut last_err = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let backoff = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                tracing::warn!(attempt, ?backoff, "retrying Anthropic request");
                tokio::time::sleep(backoff).await;
            }

            let resp = self
                .http
                .post(ANTHROPIC_API_URL)
                .header("x-api-key", self.api_key.expose_secret())
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let ant_resp: AntResponse =
                        r.json().await.context("deserialise Anthropic response")?;
                    return parse_ant_response(ant_resp);
                }
                Ok(r)
                    if r.status().as_u16() == 429
                        || r.status().as_u16() == 529
                        || r.status().is_server_error() =>
                {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    tracing::warn!(%status, body = %text, "retryable error from Anthropic");
                    last_err = Some(anyhow::anyhow!("HTTP {status}: {text}"));
                }
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    anyhow::bail!("Anthropic non-retryable error HTTP {status}: {text}");
                }
                Err(e) => {
                    last_err = Some(e.into());
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Anthropic request failed after retries")))
    }

    /// Streaming version — sends SSE request, returns a channel of StreamEvents.
    async fn do_chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let system_text: String = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let ant_messages: Vec<AntMessage<'_>> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|msg| to_ant_message(msg))
            .collect();

        let ant_tools: Vec<AntTool<'_>> = tools.iter().map(|tool| to_ant_tool(tool)).collect();

        let mut body = AntRequest {
            model: self.model.as_str(),
            max_tokens: self.max_tokens,
            temperature: Some(self.temperature),
            system: if system_text.is_empty() {
                None
            } else {
                Some(system_text)
            },
            messages: ant_messages,
            tools: if ant_tools.is_empty() {
                None
            } else {
                Some(ant_tools)
            },
            stream: Some(true),
        };

        if body.messages.is_empty() {
            body.messages.push(AntMessage {
                role: "user",
                content: AntContent::Text(Cow::Borrowed("(no user input)")),
            });
        }

        let resp = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic streaming error HTTP {status}: {text}");
        }

        let (tx, rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let mut full_text = String::new();
            let mut tc_acc: std::collections::HashMap<usize, (String, String, String)> =
                std::collections::HashMap::new();
            let mut response = resp;
            let mut done = false;
            const MAX_BUFFER_SIZE: usize = 512 * 1024; // 512 KB safety limit

            while !done {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        buffer.extend_from_slice(&chunk);
                        if buffer.len() > MAX_BUFFER_SIZE {
                            tracing::warn!("Anthropic SSE buffer exceeded limit, truncating");
                            break;
                        }
                    }
                    _ => break,
                }

                for line in drain_complete_sse_lines(&mut buffer) {
                    let data = match line.strip_prefix("data: ") {
                        Some(d) => d,
                        None => continue,
                    };

                    let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };

                    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    match event_type {
                        "content_block_delta" => {
                            let delta = &event["delta"];
                            let delta_type =
                                delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match delta_type {
                                "text_delta" => {
                                    if let Some(text) = delta.get("text").and_then(|v| v.as_str())
                                        && !text.is_empty()
                                    {
                                        full_text.push_str(text);
                                        let _ = tx.send(StreamEvent::Delta(text.to_string())).await;
                                    }
                                }
                                "input_json_delta" => {
                                    let index =
                                        event.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as usize;
                                    if let Some(partial) =
                                        delta.get("partial_json").and_then(|v| v.as_str())
                                    {
                                        tc_acc.entry(index).or_default().2.push_str(partial);
                                    }
                                }
                                _ => {}
                            }
                        }
                        "content_block_start" => {
                            let block = &event["content_block"];
                            let block_type =
                                block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if block_type == "tool_use" {
                                let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                    as usize;
                                let id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                tc_acc.insert(index, (id, name, String::new()));
                            }
                        }
                        "message_stop" => {
                            done = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }

            // Build tool calls
            let mut tool_calls = Vec::new();
            let mut keys: Vec<usize> = tc_acc.keys().copied().collect();
            keys.sort();
            for k in keys {
                let (id, name, args) = match tc_acc.remove(&k) {
                    Some(v) => v,
                    None => continue,
                };
                let arguments = serde_json::from_str(&args).unwrap_or(serde_json::Value::Null);
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }

            let resp = LlmResponse {
                text: if full_text.is_empty() {
                    None
                } else {
                    Some(full_text)
                },
                tool_calls,
            };
            let _ = tx.send(StreamEvent::Done(resp)).await;
        });

        Ok(rx)
    }
}

impl LlmClient for AnthropicClient {
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

fn drain_complete_sse_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cursor = 0;

    while let Some(rel_pos) = buffer[cursor..].iter().position(|&byte| byte == b'\n') {
        let pos = cursor + rel_pos;
        let line_bytes = if pos > cursor && buffer[pos - 1] == b'\r' {
            &buffer[cursor..pos - 1]
        } else {
            &buffer[cursor..pos]
        };

        match std::str::from_utf8(line_bytes) {
            Ok(line) => lines.push(line.to_owned()),
            Err(err) => tracing::warn!(?err, "dropping invalid UTF-8 Anthropic SSE line"),
        }

        cursor = pos + 1;
    }

    if cursor > 0 {
        buffer.drain(..cursor);
    }

    lines
}

// ─── Anthropic wire types (private) ──────────────────────────────────────────

// ── Request ──

#[derive(Serialize)]
struct AntRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AntMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AntTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct AntMessage<'a> {
    role: &'static str,
    content: AntContent<'a>,
}

/// Anthropic content can be a simple string or an array of content blocks.
#[derive(Serialize)]
#[serde(untagged)]
enum AntContent<'a> {
    Text(Cow<'a, str>),
    Blocks(Vec<AntContentBlock<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AntContentBlock<'a> {
    #[serde(rename = "text")]
    Text { text: Cow<'a, str> },
    #[serde(rename = "image")]
    Image { source: AntImageSource<'a> },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: Cow<'a, str>,
        content: Cow<'a, str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Serialize)]
struct AntImageSource<'a> {
    #[serde(rename = "type")]
    source_type: &'static str, // "base64"
    media_type: Cow<'a, str>, // e.g. "image/jpeg"
    data: Cow<'a, str>,       // base64-encoded image bytes
}

#[derive(Serialize)]
struct AntTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

// ── Response ──

#[derive(Deserialize)]
struct AntResponse {
    content: Vec<AntResponseBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AntResponseBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

// ─── Mapping helpers ─────────────────────────────────────────────────────────

fn to_ant_message(msg: &ChatMessage) -> AntMessage<'_> {
    match msg.role {
        Role::System => {
            // Should not reach here (filtered out above), but handle gracefully.
            AntMessage {
                role: "user",
                content: AntContent::Text(Cow::Borrowed(msg.content.as_str())),
            }
        }
        Role::User => {
            // Check if this user message has image attachments
            if let Some(ref images) = msg.images {
                let mut blocks = Vec::new();
                for img in images {
                    blocks.push(AntContentBlock::Image {
                        source: AntImageSource {
                            source_type: "base64",
                            media_type: Cow::Borrowed(img.media_type.as_str()),
                            data: Cow::Borrowed(img.data.as_str()),
                        },
                    });
                }
                if !msg.content.is_empty() {
                    blocks.push(AntContentBlock::Text {
                        text: Cow::Borrowed(msg.content.as_str()),
                    });
                }
                AntMessage {
                    role: "user",
                    content: AntContent::Blocks(blocks),
                }
            } else {
                AntMessage {
                    role: "user",
                    content: AntContent::Text(Cow::Borrowed(msg.content.as_str())),
                }
            }
        }
        Role::Assistant => {
            match &msg.tool_calls {
                Some(calls) if !calls.is_empty() => {
                    // Assistant message with tool_use blocks.
                    let mut blocks = Vec::new();
                    if !msg.content.is_empty() {
                        blocks.push(AntContentBlock::Text {
                            text: Cow::Borrowed(msg.content.as_str()),
                        });
                    }
                    for call in calls {
                        blocks.push(AntContentBlock::ToolUse {
                            id: call.id.as_str(),
                            name: call.name.as_str(),
                            input: &call.arguments,
                        });
                    }
                    AntMessage {
                        role: "assistant",
                        content: AntContent::Blocks(blocks),
                    }
                }
                _ => AntMessage {
                    role: "assistant",
                    content: AntContent::Text(Cow::Borrowed(msg.content.as_str())),
                },
            }
        }
        Role::Tool => {
            // Tool result → user message with tool_result content block.
            // Anthropic requires tool results to be sent as a "user" role message.
            let block = AntContentBlock::ToolResult {
                tool_use_id: msg
                    .tool_call_id
                    .as_deref()
                    .map(Cow::Borrowed)
                    .unwrap_or_default(),
                content: Cow::Borrowed(msg.content.as_str()),
                is_error: None,
            };
            AntMessage {
                role: "user",
                content: AntContent::Blocks(vec![block]),
            }
        }
    }
}

fn to_ant_tool(def: &ToolDefinition) -> AntTool<'_> {
    AntTool {
        name: def.name.as_str(),
        description: def.description.as_str(),
        input_schema: &def.parameters,
    }
}

fn parse_ant_response(resp: AntResponse) -> Result<LlmResponse> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in resp.content {
        match block {
            AntResponseBlock::Text { text } => {
                text_parts.push(text);
            }
            AntResponseBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: input,
                });
            }
        }
    }

    let text = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    };

    Ok(LlmResponse { text, tool_calls })
}

#[cfg(test)]
mod tests {
    use super::drain_complete_sse_lines;

    #[test]
    fn drain_complete_sse_lines_handles_split_utf8_across_chunks() {
        let mut buffer = b"data: {\"text\":\"".to_vec();
        buffer.extend_from_slice(&[0xE4, 0xBD]);

        assert!(drain_complete_sse_lines(&mut buffer).is_empty());

        buffer.extend_from_slice(&[0xA0, b'"', b'}', b'\n']);

        assert_eq!(
            drain_complete_sse_lines(&mut buffer),
            vec![format!("data: {{\"text\":\"{}\"}}", '\u{4F60}')]
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn drain_complete_sse_lines_preserves_partial_tail() {
        let mut buffer = b"data: first\ndata: second".to_vec();

        assert_eq!(drain_complete_sse_lines(&mut buffer), vec!["data: first"]);
        assert_eq!(buffer, b"data: second");
    }

    #[test]
    fn drain_complete_sse_lines_trims_crlf() {
        let mut buffer = b"data: first\r\n".to_vec();

        assert_eq!(drain_complete_sse_lines(&mut buffer), vec!["data: first"]);
        assert!(buffer.is_empty());
    }
}
