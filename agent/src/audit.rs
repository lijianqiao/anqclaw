//! Structured audit logging — records tool calls, commands, and file operations.
//!
//! Writes JSONL (one JSON object per line) to the configured audit log file.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use serde::Serialize;

// ─── Audit Event Types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(tag = "event_type")]
pub enum AuditEvent {
    #[serde(rename = "tool_call")]
    ToolCall {
        timestamp: String,
        trace_id: String,
        chat_id: String,
        tool_name: String,
        arguments: serde_json::Value,
        result_preview: String,
        is_error: bool,
        duration_ms: u64,
    },
    #[serde(rename = "llm_call")]
    LlmCall {
        timestamp: String,
        trace_id: String,
        chat_id: String,
        model: String,
        input_messages: usize,
        has_tool_calls: bool,
        has_text: bool,
        duration_ms: u64,
    },
}

// ─── AuditLogger ────────────────────────────────────────────────────────────

pub struct AuditLogger {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl AuditLogger {
    /// Create a new audit logger that writes to the given file path.
    /// Creates parent directories if needed.
    pub fn new(log_file: &str) -> anyhow::Result<Self> {
        let path = Path::new(log_file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self {
            writer: Mutex::new(Box::new(file)),
        })
    }

    /// Log a tool call event.
    pub fn log_tool_call(
        &self,
        trace_id: &str,
        chat_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        result: &str,
        is_error: bool,
        duration_ms: u64,
    ) {
        let preview = if result.len() > 500 {
            format!("{}...[truncated]", &result[..500])
        } else {
            result.to_string()
        };

        let event = AuditEvent::ToolCall {
            timestamp: Utc::now().to_rfc3339(),
            trace_id: trace_id.to_string(),
            chat_id: chat_id.to_string(),
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            result_preview: preview,
            is_error,
            duration_ms,
        };

        self.write_event(&event);
    }

    /// Log an LLM call event.
    pub fn log_llm_call(
        &self,
        trace_id: &str,
        chat_id: &str,
        model: &str,
        input_messages: usize,
        has_tool_calls: bool,
        has_text: bool,
        duration_ms: u64,
    ) {
        let event = AuditEvent::LlmCall {
            timestamp: Utc::now().to_rfc3339(),
            trace_id: trace_id.to_string(),
            chat_id: chat_id.to_string(),
            model: model.to_string(),
            input_messages,
            has_tool_calls,
            has_text,
            duration_ms,
        };

        self.write_event(&event);
    }

    fn write_event(&self, event: &AuditEvent) {
        if let Ok(json) = serde_json::to_string(event)
            && let Ok(mut writer) = self.writer.lock()
        {
            let _ = writeln!(writer, "{json}");
            let _ = writer.flush();
        }
    }
}
