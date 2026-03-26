//! Feishu WebSocket protocol types.
//!
//! The Feishu WS long-connection uses a custom Protobuf binary frame protocol
//! (pbbp2), NOT plain JSON over WebSocket text frames.
//!
//! Reference: zeroclaw/lark.rs

use crate::types::{InboundMessage, MessageContent};

// ─── Protobuf Frame Types ────────────────────────────────────────────────────

/// Protobuf header key-value pair within a PbFrame.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PbHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

/// Feishu WS frame (pbbp2.proto).
///
/// - `method = 0` → CONTROL frame (ping/pong)
/// - `method = 1` → DATA frame (events)
#[derive(Clone, PartialEq, prost::Message)]
pub struct PbFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<PbHeader>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
}

impl PbFrame {
    /// Looks up a header value by key. Returns "" if not found.
    pub fn header_value(&self, key: &str) -> &str {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
            .unwrap_or("")
    }
}

// ─── WS Endpoint Response ────────────────────────────────────────────────────

/// Response from `POST /callback/ws/endpoint`.
#[derive(Debug, serde::Deserialize)]
pub struct WsEndpointResp {
    pub code: i32,
    #[serde(default)]
    pub msg: Option<String>,
    #[serde(default)]
    pub data: Option<WsEndpoint>,
}

#[derive(Debug, serde::Deserialize)]
pub struct WsEndpoint {
    #[serde(rename = "URL")]
    pub url: String,
    #[serde(rename = "ClientConfig")]
    pub client_config: Option<WsClientConfig>,
}

/// Server-sent client config (parsed from pong payload).
#[derive(Debug, serde::Deserialize, Default, Clone)]
pub struct WsClientConfig {
    #[serde(rename = "PingInterval")]
    pub ping_interval: Option<u64>,
}

// ─── Event Types ─────────────────────────────────────────────────────────────

/// Top-level Lark event envelope (extracted from DATA frame payload).
#[derive(Debug, serde::Deserialize)]
pub struct LarkEvent {
    pub header: LarkEventHeader,
    pub event: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
pub struct LarkEventHeader {
    pub event_type: String,
    #[allow(dead_code)]
    pub event_id: String,
}

/// Payload for `im.message.receive_v1` events.
#[derive(Debug, serde::Deserialize)]
pub struct MsgReceivePayload {
    pub sender: LarkSender,
    pub message: LarkMessage,
}

#[derive(Debug, serde::Deserialize)]
pub struct LarkSender {
    pub sender_id: LarkSenderId,
    #[serde(default)]
    pub sender_type: String,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct LarkSenderId {
    pub open_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct LarkMessage {
    pub message_id: String,
    pub chat_id: String,
    #[allow(dead_code)]
    pub chat_type: String,
    pub message_type: String,
    #[serde(default)]
    pub content: String,
}

// ─── Conversion ──────────────────────────────────────────────────────────────

impl MsgReceivePayload {
    /// Converts a Feishu message event into our `InboundMessage`.
    ///
    /// Parses `message_type` to determine `MessageContent` variant:
    /// - `text`  → `MessageContent::Text`
    /// - `image` → `MessageContent::Image`
    /// - `file`  → `MessageContent::File`
    /// - `post`  → `MessageContent::RichText`
    /// - other   → `MessageContent::Text("[unsupported: {type}]")`
    pub fn into_inbound(self) -> Option<InboundMessage> {
        let sender_id = self.sender.sender_id.open_id.unwrap_or_default();
        let lark_msg = self.message;

        let content = match lark_msg.message_type.as_str() {
            "text" => {
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content).ok()?;
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let text = strip_at_placeholders(text).trim().to_string();
                if text.is_empty() {
                    return None;
                }
                MessageContent::Text(text)
            }
            "image" => {
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content).ok()?;
                let key = v.get("image_key").and_then(|k| k.as_str())?.to_string();
                MessageContent::Image { key, image_data: None }
            }
            "file" => {
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content).ok()?;
                let key = v.get("file_key").and_then(|k| k.as_str())?.to_string();
                let name = v
                    .get("file_name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                MessageContent::File { key, name, file_bytes: None }
            }
            "post" => {
                // Rich text (post) — extract plain text from structured content
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content).ok()?;
                let text = extract_post_text(&v);
                if text.trim().is_empty() {
                    return None;
                }
                MessageContent::RichText(v)
            }
            other => {
                MessageContent::Text(format!("[不支持的消息类型: {other}]"))
            }
        };

        Some(InboundMessage {
            channel: "feishu".into(),
            chat_id: lark_msg.chat_id,
            sender_id,
            message_id: lark_msg.message_id,
            content,
            timestamp: chrono::Utc::now().timestamp(),
            trace_id: String::new(),
            images: vec![],
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Strips `@_user_N` placeholders that Feishu inserts for @mentions.
fn strip_at_placeholders(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '@' {
            // Check if followed by _user_ pattern
            let rest: String = chars.clone().take(6).collect();
            if rest.starts_with("_user_") {
                // Skip @_user_N (consume _user_ + digits)
                for _ in 0..6 {
                    chars.next();
                }
                // Skip trailing digits
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                    chars.next();
                }
                // Skip one trailing space if present
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                continue;
            }
        }
        result.push(ch);
    }
    result
}

/// Extracts plain text from a Feishu "post" (rich text) content JSON.
///
/// Post format: `{ "zh_cn": { "content": [[{"tag": "text", "text": "..."}, ...]] } }`
fn extract_post_text(v: &serde_json::Value) -> String {
    let mut text = String::new();

    // Try common locale keys
    let locales = ["zh_cn", "en_us", "ja_jp", "zh_hk", "zh_tw"];
    let post = locales
        .iter()
        .find_map(|locale| v.get(locale))
        .or_else(|| {
            // Fall back to first key
            v.as_object().and_then(|obj| obj.values().next())
        });

    if let Some(post) = post
        && let Some(content) = post.get("content").and_then(|c| c.as_array())
    {
        for paragraph in content {
            if let Some(elements) = paragraph.as_array() {
                for elem in elements {
                    if let Some(t) = elem.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                }
                text.push('\n');
            }
        }
    }

    text
}

/// Max byte size for a single interactive card's markdown content.
/// Lark card payloads have a ~30 KB limit; leave margin for JSON envelope.
pub const CARD_MARKDOWN_MAX_BYTES: usize = 28_000;

/// Build an interactive card JSON string with a single markdown element.
pub fn build_card_content(markdown: &str) -> String {
    serde_json::json!({
        "schema": "2.0",
        "body": {
            "elements": [{
                "tag": "markdown",
                "content": markdown
            }]
        }
    })
    .to_string()
}

/// Split markdown content into chunks that fit within the card size limit.
/// Splits on line boundaries to avoid breaking markdown syntax.
pub fn split_markdown_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.len() <= max_bytes {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        if start + max_bytes >= text.len() {
            chunks.push(&text[start..]);
            break;
        }

        let end = start + max_bytes;
        let search_region = &text[start..end];
        // Try to split at a newline boundary
        let split_at = search_region
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        // Ensure we're at a char boundary
        let split_at = if text.is_char_boundary(split_at) {
            split_at
        } else {
            (start..split_at)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(start)
        };

        if split_at <= start {
            // Force split forward
            let forced = (end..=text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(text.len());
            chunks.push(&text[start..forced]);
            start = forced;
        } else {
            chunks.push(&text[start..split_at]);
            start = split_at;
        }
    }

    chunks
}
