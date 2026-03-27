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
    /// - `post`  → `MessageContent::Text` (extracted as markdown text)
    /// - other   → `MessageContent::Text` (raw content as fallback)
    ///
    /// Returns `Err` with a reason string when the message cannot be converted,
    /// so the caller can log the cause instead of silently dropping it.
    pub fn into_inbound(self) -> Result<InboundMessage, String> {
        let sender_id = self.sender.sender_id.open_id.unwrap_or_default();
        let lark_msg = self.message;
        let msg_type = lark_msg.message_type.as_str();

        let content = match msg_type {
            "text" => {
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content)
                    .map_err(|e| format!("text content parse failed: {e}"))?;
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let text = strip_at_placeholders(text).trim().to_string();
                if text.is_empty() {
                    return Err("text content is empty after stripping mentions".into());
                }
                MessageContent::Text(text)
            }
            "image" => {
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content)
                    .map_err(|e| format!("image content parse failed: {e}"))?;
                let key = v.get("image_key").and_then(|k| k.as_str())
                    .ok_or("image content missing image_key")?
                    .to_string();
                MessageContent::Image { key, image_data: None }
            }
            "file" => {
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content)
                    .map_err(|e| format!("file content parse failed: {e}"))?;
                let key = v.get("file_key").and_then(|k| k.as_str())
                    .ok_or("file content missing file_key")?
                    .to_string();
                let name = v
                    .get("file_name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                MessageContent::File { key, name, file_bytes: None }
            }
            "post" => {
                // Rich text (post) — extract plain text from structured content.
                // Feishu markdown input often arrives as "post" type with markdown
                // elements inside the content array.
                let v: serde_json::Value = serde_json::from_str(&lark_msg.content)
                    .map_err(|e| format!("post content parse failed: {e}"))?;
                let text = extract_post_text(&v);
                if text.trim().is_empty() {
                    // Fallback: the structured extraction missed content (e.g.
                    // unknown element tags). Try the raw JSON string so we don't
                    // silently discard user messages.
                    let fallback = extract_fallback_text(&lark_msg.content);
                    if fallback.is_empty() {
                        return Err(format!(
                            "post content is empty after extraction, raw: {}",
                            truncate_str(&lark_msg.content, 300),
                        ));
                    }
                    tracing::warn!(
                        raw_content = %truncate_str(&lark_msg.content, 200),
                        "Feishu: post text extraction empty, using fallback"
                    );
                    MessageContent::Text(fallback)
                } else {
                    // Normalize to Text so downstream (agent/LLM) never needs
                    // to understand channel-specific rich text JSON structures.
                    MessageContent::Text(text)
                }
            }
            _ => {
                // Unknown message type — try to extract any usable text from raw
                // content rather than discarding it.
                let fallback = extract_fallback_text(&lark_msg.content);
                if fallback.is_empty() {
                    return Err(format!(
                        "unsupported message_type '{msg_type}', raw content: {}",
                        truncate_str(&lark_msg.content, 200),
                    ));
                }
                tracing::info!(
                    msg_type,
                    "Feishu: extracted text from unsupported message type"
                );
                MessageContent::Text(fallback)
            }
        };

        Ok(InboundMessage {
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
/// Feishu sends post content in two possible shapes:
/// 1. Locale-wrapped: `{ "zh_cn": { "content": [[...]] } }`
/// 2. Flat (no locale): `{ "title": "...", "content": [[...]] }`
///
/// Handles element tags: text, markdown, a, code_block. Skips: at, img, etc.
fn extract_post_text(v: &serde_json::Value) -> String {
    // Locate the content array — try two structures:
    // 1. Top-level "content" (flat / no locale wrapper)
    // 2. Under a locale key like "zh_cn"
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .or_else(|| {
            let locales = ["zh_cn", "en_us", "ja_jp", "zh_hk", "zh_tw"];
            locales
                .iter()
                .find_map(|locale| v.get(locale))
                .or_else(|| {
                    // Fall back to first object value that has "content"
                    v.as_object()
                        .and_then(|obj| obj.values().find(|val| val.get("content").is_some()))
                })
                .and_then(|post| post.get("content"))
                .and_then(|c| c.as_array())
        });

    let Some(content) = content else {
        return String::new();
    };

    let mut text = String::new();
    for paragraph in content {
        let Some(elements) = paragraph.as_array() else {
            continue;
        };
        for elem in elements {
            let tag = elem.get("tag").and_then(|t| t.as_str()).unwrap_or("");
            match tag {
                "text" | "markdown" => {
                    if let Some(t) = elem.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                    if let Some(t) = elem.get("content").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                }
                "a" => {
                    if let Some(t) = elem.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                }
                "code_block" => {
                    let lang = elem
                        .get("language")
                        .and_then(|l| l.as_str())
                        .unwrap_or("");
                    // Map Feishu's "PLAIN_TEXT" to empty, keep real languages
                    let lang_tag = if lang.is_empty() || lang.eq_ignore_ascii_case("PLAIN_TEXT") {
                        ""
                    } else {
                        lang
                    };
                    if let Some(t) = elem.get("text").and_then(|t| t.as_str()) {
                        text.push_str("```");
                        text.push_str(lang_tag);
                        text.push('\n');
                        text.push_str(t);
                        if !t.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str("```");
                    }
                }
                // Skip: "at", "img", "media", "emotion" etc.
                _ => {}
            }
        }
        text.push('\n');
    }

    text
}

/// Try to extract usable text from raw content JSON of unknown message types.
/// Looks for common fields: "text", "content" (string or post-array), "title".
fn extract_fallback_text(raw_content: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(raw_content) {
        Ok(v) => v,
        Err(_) => {
            // Not JSON — use raw string if it looks like text
            let trimmed = raw_content.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('\0') {
                return trimmed.to_string();
            }
            return String::new();
        }
    };

    // Try string fields first
    for key in &["text", "title"] {
        if let Some(s) = v.get(key).and_then(|t| t.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }

    // "content" might be a string OR a post-style array — try both
    if let Some(content) = v.get("content") {
        if let Some(s) = content.as_str() {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
        // Try as post-style content array
        let post_text = extract_post_text(&v);
        if !post_text.trim().is_empty() {
            return post_text;
        }
    }

    String::new()
}

/// Truncate a string to at most `max` bytes at a char boundary.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat post (no locale wrapper) — this is what Feishu actually sends
    /// when a user types markdown with code blocks in the chat box.
    #[test]
    fn test_extract_post_text_flat_with_code_block() {
        let v: serde_json::Value = serde_json::json!({
            "title": "",
            "content": [[
                {"tag": "text", "text": "请阅读以下代码：", "style": []},
                {"tag": "code_block", "language": "Python", "text": "def foo():\n    pass"}
            ]]
        });
        let text = extract_post_text(&v);
        assert!(text.contains("请阅读以下代码："), "should contain text element");
        assert!(text.contains("```Python"), "should contain language tag");
        assert!(text.contains("def foo():\n    pass"), "should contain code");
        assert!(text.contains("```"), "should close code block");
    }

    /// Locale-wrapped post (classic format).
    #[test]
    fn test_extract_post_text_locale_wrapped() {
        let v: serde_json::Value = serde_json::json!({
            "zh_cn": {
                "content": [[
                    {"tag": "text", "text": "你好"},
                    {"tag": "a", "text": "链接", "href": "https://example.com"}
                ]]
            }
        });
        let text = extract_post_text(&v);
        assert!(text.contains("你好"));
        assert!(text.contains("链接"));
    }

    /// code_block with PLAIN_TEXT language should not emit a language tag.
    #[test]
    fn test_extract_post_text_code_block_plain_text() {
        let v: serde_json::Value = serde_json::json!({
            "content": [[
                {"tag": "code_block", "language": "PLAIN_TEXT", "text": "hello world"}
            ]]
        });
        let text = extract_post_text(&v);
        assert!(text.contains("```\n"), "PLAIN_TEXT should produce bare ```");
        assert!(!text.contains("```PLAIN_TEXT"));
    }

    /// Fallback should handle content as a post-style array.
    #[test]
    fn test_extract_fallback_text_post_array() {
        let raw = r#"{"title":"","content":[[{"tag":"text","text":"hello from fallback"}]]}"#;
        let text = extract_fallback_text(raw);
        assert!(text.contains("hello from fallback"));
    }

    /// into_inbound should succeed for the exact payload from the bug report.
    #[test]
    fn test_into_inbound_flat_post_with_code_block() {
        let payload = MsgReceivePayload {
            sender: LarkSender {
                sender_id: LarkSenderId { open_id: Some("ou_test".into()) },
                sender_type: "user".into(),
            },
            message: LarkMessage {
                message_id: "om_test".into(),
                chat_id: "oc_test".into(),
                chat_type: "p2p".into(),
                message_type: "post".into(),
                content: serde_json::json!({
                    "title": "",
                    "content": [[
                        {"tag": "at", "user_id": "@_user_1", "user_name": "anqclaw", "style": []},
                        {"tag": "text", "text": " 请阅读以下代码：", "style": []},
                        {"tag": "code_block", "language": "PLAIN_TEXT", "text": "def func(nums):\n    pass"}
                    ]]
                }).to_string(),
            },
        };
        let result = payload.into_inbound();
        assert!(result.is_ok(), "should not drop the message: {:?}", result.err());
        let msg = result.unwrap();
        match &msg.content {
            MessageContent::Text(t) => {
                assert!(t.contains("请阅读以下代码"), "text: {t}");
                assert!(t.contains("def func(nums)"), "should contain code: {t}");
            }
            other => panic!("expected Text, got: {other:?}"),
        }
    }
}
