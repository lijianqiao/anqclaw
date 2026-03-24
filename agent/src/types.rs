use serde::{Deserialize, Serialize};

// ─── Inbound Message ────────────────────────────────────────────────────────

/// Inbound message (from channel to gateway)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub sender_id: String,
    pub message_id: String,
    pub content: MessageContent,
    pub timestamp: i64,
}

impl InboundMessage {
    /// Creates a heartbeat virtual message with the given prompt text.
    pub fn heartbeat(prompt: &str) -> Self {
        Self {
            channel: "__heartbeat__".into(),
            chat_id: "__heartbeat__".into(),
            sender_id: "__system__".into(),
            message_id: String::new(),
            content: MessageContent::Text(prompt.to_string()),
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

// ─── Message Content ─────────────────────────────────────────────────────────

/// Message content — receiving supports multiple types; sending only uses Text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum MessageContent {
    Text(String),
    Image { key: String },
    File { key: String, name: String },
    RichText(serde_json::Value),
}

impl MessageContent {
    /// Returns a plain-text representation of the content.
    pub fn to_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Image { key } => format!("[图片: {}]", key),
            MessageContent::File { key: _, name } => format!("[文件: {}]", name),
            MessageContent::RichText(_) => "[富文本消息]".to_string(),
        }
    }
}

// ─── Outbound Message ────────────────────────────────────────────────────────

/// Outbound message (from gateway to channel)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub reply_to: Option<String>,
    pub content: String,
}

impl OutboundMessage {
    /// Creates an error reply targeting the same chat/channel as the inbound message.
    pub fn error(msg: &InboundMessage, error: &str) -> Self {
        Self {
            channel: msg.channel.clone(),
            chat_id: msg.chat_id.clone(),
            reply_to: if msg.message_id.is_empty() {
                None
            } else {
                Some(msg.message_id.clone())
            },
            content: error.to_string(),
        }
    }
}

// ─── Role ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

// ─── Tool Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ─── Chat Message ────────────────────────────────────────────────────────────

/// LLM conversation message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// Creates a System role message.
    pub fn system(content: &str) -> Self {
        Self {
            role: Role::System,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Creates a User role message.
    pub fn user(content: &str) -> Self {
        Self {
            role: Role::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Creates an Assistant role message with no tool calls.
    pub fn assistant(content: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Creates an Assistant role message that includes tool calls.
    pub fn assistant_with_tools(text: Option<&str>, calls: &[ToolCall]) -> Self {
        Self {
            role: Role::Assistant,
            content: text.unwrap_or("").to_string(),
            tool_calls: Some(calls.to_vec()),
            tool_call_id: None,
        }
    }

    /// Creates a Tool role message from a ToolResult.
    pub fn tool_result(result: &ToolResult) -> Self {
        Self {
            role: Role::Tool,
            content: result.output.clone(),
            tool_calls: None,
            tool_call_id: Some(result.call_id.clone()),
        }
    }
}

// ─── LLM Response ────────────────────────────────────────────────────────────

/// An LLM response may contain both a text reply and tool calls simultaneously.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}
