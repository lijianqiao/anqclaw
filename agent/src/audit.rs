//! Structured audit logging — records tool calls, commands, and file operations.
//!
//! Writes JSONL (one JSON object per line) to the configured audit log file.

use std::io::{BufWriter, Write};
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

const AUDIT_BUFFER_CAPACITY: usize = 64 * 1024;
const AUDIT_FLUSH_EVERY: usize = 16;

struct AuditWriter {
    writer: BufWriter<std::fs::File>,
    pending_events: usize,
}

pub struct AuditLogger {
    writer: Mutex<AuditWriter>,
}

impl AuditLogger {
    fn preview_text(text: &str, max_chars: usize) -> String {
        let mut chars = text.chars();
        let preview: String = chars.by_ref().take(max_chars).collect();
        if chars.next().is_some() {
            format!("{preview}...[truncated]")
        } else {
            preview
        }
    }

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
            writer: Mutex::new(AuditWriter {
                writer: BufWriter::with_capacity(AUDIT_BUFFER_CAPACITY, file),
                pending_events: 0,
            }),
        })
    }

    /// Log a tool call event.
    #[allow(clippy::too_many_arguments)]
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
        let preview = Self::preview_text(result, 500);

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
    #[allow(clippy::too_many_arguments)]
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
        let json = match serde_json::to_string(event) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "audit: failed to serialize event / 审计: 序列化事件失败");
                return;
            }
        };
        let mut writer = match self.writer.lock() {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "audit: failed to acquire writer lock / 审计: 获取写入锁失败");
                return;
            }
        };
        if let Err(e) = writeln!(writer.writer, "{json}") {
            tracing::warn!(error = %e, "audit: failed to write event / 审计: 写入事件失败");
            return;
        }

        writer.pending_events += 1;
        if writer.pending_events >= AUDIT_FLUSH_EVERY {
            if let Err(e) = writer.writer.flush() {
                tracing::warn!(error = %e, "audit: failed to flush writer / 审计: 刷新写入器失败");
                return;
            }
            writer.pending_events = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AuditLogger;

    #[test]
    fn test_preview_text_preserves_char_boundaries() {
        let text = "设备数据导出.xlsx设备数据导出.xlsx设备数据导出.xlsx";
        let preview = AuditLogger::preview_text(text, 10);
        assert_eq!(preview.chars().take(10).count(), 10);
        assert!(!preview.contains(''));
        assert!(preview.ends_with("...[truncated]"));
    }
}
