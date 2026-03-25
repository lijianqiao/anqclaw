//! Agent Core — agentic loop with LLM ↔ tool calling.
//!
//! TODO(future): When splitting into workspace crates, extract into
//! `crates/agent/` with its own `Cargo.toml`.

pub mod context;
pub mod prompt;
pub mod redact;

use std::sync::Arc;

use anyhow::Result;

use crate::audit::AuditLogger;
use crate::config::AppConfig;
use crate::llm::LlmClient;
use crate::memory::MemoryStore;
use crate::tool::ToolRegistry;
use crate::types::{ChatMessage, InboundMessage, OutboundMessage};

use context::{build_system_prompt, format_memories};

// ─── AgentCore ───────────────────────────────────────────────────────────────

pub struct AgentCore {
    llm: Arc<dyn LlmClient>,
    fallback_llm: Option<Arc<dyn LlmClient>>,
    tools: Arc<ToolRegistry>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
    /// Cached secret values for redaction
    secrets: Vec<String>,
    audit: Option<Arc<AuditLogger>>,
}

impl AgentCore {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        fallback_llm: Option<Arc<dyn LlmClient>>,
        tools: Arc<ToolRegistry>,
        memory: Arc<MemoryStore>,
        config: Arc<AppConfig>,
        audit: Option<Arc<AuditLogger>>,
    ) -> Self {
        let secrets = if config.security.auto_redact_secrets {
            redact::collect_secrets(&config)
        } else {
            vec![]
        };
        Self {
            llm,
            fallback_llm,
            tools,
            memory,
            config,
            secrets,
            audit,
        }
    }

    /// Handles an inbound message through the full agentic loop.
    ///
    /// Returns `(OutboundMessage, Vec<ChatMessage>)` — the reply and the full
    /// conversation slice (including tool call rounds) that should be persisted.
    pub async fn handle(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
    ) -> (OutboundMessage, Vec<ChatMessage>) {
        match self.do_handle(msg, history).await {
            Ok((reply, messages)) => (reply, messages),
            Err(e) => {
                tracing::error!(error = %e, "agent handle failed");
                let reply = OutboundMessage::error(msg, &format!("处理失败: {e}"));
                (reply, vec![])
            }
        }
    }

    async fn do_handle(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
    ) -> Result<(OutboundMessage, Vec<ChatMessage>)> {
        // 1. Build system prompt
        let system_prompt = build_system_prompt(&self.config);

        // 2. Search relevant memories
        let user_text = msg.content.to_text();
        let memories = self
            .memory
            .search_memory(&user_text, self.config.memory.search_limit as usize)
            .await
            .unwrap_or_default();

        // 3. Assemble messages
        let mut messages: Vec<ChatMessage> = Vec::new();

        // System prompt
        messages.push(ChatMessage::system(&system_prompt));

        // Inject relevant memories
        let mem_text = format_memories(&memories);
        if !mem_text.is_empty() {
            messages.push(ChatMessage::system(&mem_text));
        }

        // History (from SQLite)
        messages.extend_from_slice(history);

        // Current user message
        let user_msg = ChatMessage::user(&user_text);
        messages.push(user_msg);

        // Track new messages for persistence (everything after history)
        // We'll collect all new messages (user + assistant + tool rounds)
        let persist_start = messages.len() - 1; // index of user message

        // 4. Get tool definitions
        let tool_defs = self.tools.definitions();

        // 5. Agentic loop
        let max_rounds = self.config.agent.max_tool_rounds;
        for round in 0..max_rounds {
            let llm_start = std::time::Instant::now();
            let response = match self.llm.chat(&messages, &tool_defs).await {
                Ok(r) => r,
                Err(e) => {
                    if let Some(ref fallback) = self.fallback_llm {
                        tracing::warn!(error = %e, "primary LLM failed, trying fallback");
                        fallback.chat(&messages, &tool_defs).await?
                    } else {
                        return Err(e);
                    }
                }
            };
            let llm_duration_ms = llm_start.elapsed().as_millis() as u64;

            let has_tool_calls = !response.tool_calls.is_empty();
            let has_text = response.text.is_some();

            // Audit: log LLM call
            if let Some(ref audit) = self.audit
                && self.config.audit.log_llm_calls
            {
                audit.log_llm_call(
                    &msg.chat_id,
                    &self.config.llm.model,
                    messages.len(),
                    has_tool_calls,
                    has_text,
                    llm_duration_ms,
                );
            }

            if has_tool_calls {
                // Record assistant message with tool calls
                messages.push(ChatMessage::assistant_with_tools(
                    response.text.as_deref(),
                    &response.tool_calls,
                ));

                tracing::info!(
                    round,
                    tools = ?response.tool_calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    "executing tool calls"
                );

                // Execute all tool calls concurrently (with timing)
                let tools_start = std::time::Instant::now();
                let results = self.tools.execute_batch(&response.tool_calls).await;
                let tools_duration_ms = tools_start.elapsed().as_millis() as u64;

                // Audit: log each tool call
                if let Some(ref audit) = self.audit
                    && self.config.audit.log_tool_calls
                {
                    // Distribute total batch time equally as approximation;
                    // for precise per-tool timing, execute_batch would need to return durations.
                    let per_tool_ms = tools_duration_ms / results.len().max(1) as u64;
                    for (call, result) in response.tool_calls.iter().zip(results.iter()) {
                        audit.log_tool_call(
                            &msg.chat_id,
                            &call.name,
                            &call.arguments,
                            &result.output,
                            result.is_error,
                            per_tool_ms,
                        );
                    }
                }

                // Append each tool result
                for result in &results {
                    messages.push(ChatMessage::tool_result(result));
                }

                // Continue loop — let LLM see the results
            } else if has_text {
                // Pure text response — done
                let mut text = response.text.unwrap();

                // Apply redaction if enabled
                if self.config.security.auto_redact_secrets {
                    text = redact::redact_output(
                        &text,
                        &self.secrets,
                        &self.config.security.redact_patterns,
                    );
                }

                messages.push(ChatMessage::assistant(&text));

                let reply = OutboundMessage {
                    channel: msg.channel.clone(),
                    chat_id: msg.chat_id.clone(),
                    reply_to: if msg.message_id.is_empty() {
                        None
                    } else {
                        Some(msg.message_id.clone())
                    },
                    content: text,
                };

                let persist_messages = messages[persist_start..].to_vec();
                return Ok((reply, persist_messages));
            } else {
                // Empty response — treat as error
                let reply = OutboundMessage::error(msg, "LLM 返回了空响应");
                let persist_messages = messages[persist_start..].to_vec();
                return Ok((reply, persist_messages));
            }
        }

        // Exceeded max rounds
        let error_text = format!("处理超过最大轮次限制 ({max_rounds} 轮)，已停止");
        messages.push(ChatMessage::assistant(&error_text));

        let reply = OutboundMessage::error(msg, &error_text);
        let persist_messages = messages[persist_start..].to_vec();
        Ok((reply, persist_messages))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::types::{LlmResponse, MessageContent, ToolCall, ToolDefinition};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── Mock LLM Client ──────────────────────────────────────────────────────

    /// A mock LLM client that returns responses from a pre-defined sequence.
    struct MockLlm {
        responses: Vec<LlmResponse>,
        call_count: AtomicU32,
    }

    impl MockLlm {
        fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses,
                call_count: AtomicU32::new(0),
            }
        }
    }

    impl LlmClient for MockLlm {
        fn chat<'a>(
            &'a self,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDefinition],
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
            Box::pin(async {
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
                if idx < self.responses.len() {
                    Ok(self.responses[idx].clone())
                } else {
                    // Repeat last response (for max-rounds test)
                    Ok(self.responses.last().unwrap().clone())
                }
            })
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    async fn test_memory() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::new(":memory:").await.unwrap())
    }

    fn test_config() -> Arc<AppConfig> {
        let toml_str = r#"
[app]
name = "test"
workspace = "./test_workspace_nonexistent"
log_level = "info"

[feishu]
app_id = "test"
app_secret = "test"

[llm]
provider = "anthropic"
model = "test"
api_key = "test"

[agent]
max_tool_rounds = 3
"#;
        Arc::new(AppConfig::load_from_str(toml_str).unwrap())
    }

    fn test_inbound() -> InboundMessage {
        InboundMessage {
            channel: "test".into(),
            chat_id: "chat_test".into(),
            sender_id: "user_test".into(),
            message_id: "msg_test".into(),
            content: MessageContent::Text("你好".into()),
            timestamp: 0,
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_simple_text_response() {
        let memory = test_memory().await;
        let config = test_config();

        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("你好！有什么可以帮你的？".into()),
            tool_calls: vec![],
        }]));

        let tools = Arc::new(ToolRegistry::new(&config.tools, &config.security, memory.clone()));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None);

        let (reply, persist) = agent.handle(&test_inbound(), &[]).await;

        assert_eq!(reply.content, "你好！有什么可以帮你的？");
        assert_eq!(reply.channel, "test");
        // persist should contain: user msg + assistant msg
        assert_eq!(persist.len(), 2);
    }

    #[tokio::test]
    async fn test_tool_call_loop() {
        let memory = test_memory().await;
        let config = test_config();

        let mock_llm = Arc::new(MockLlm::new(vec![
            // Round 1: LLM requests a tool call
            LlmResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({"command": "date"}),
                }],
            },
            // Round 2: LLM sees tool result, returns text
            LlmResponse {
                text: Some("当前时间已获取。".into()),
                tool_calls: vec![],
            },
        ]));

        let tools = Arc::new(ToolRegistry::new(&config.tools, &config.security, memory.clone()));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None);

        let (reply, persist) = agent.handle(&test_inbound(), &[]).await;

        assert_eq!(reply.content, "当前时间已获取。");
        // persist: user + assistant(tool_call) + tool_result + assistant(text)
        assert_eq!(persist.len(), 4);
    }

    #[tokio::test]
    async fn test_max_rounds_exceeded() {
        let memory = test_memory().await;
        let config = test_config(); // max_tool_rounds = 3

        // Mock always returns tool calls — never a text reply
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_loop".into(),
                name: "shell_exec".into(),
                arguments: serde_json::json!({"command": "date"}),
            }],
        }]));

        let tools = Arc::new(ToolRegistry::new(&config.tools, &config.security, memory.clone()));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None);

        let (reply, _persist) = agent.handle(&test_inbound(), &[]).await;

        assert!(reply.content.contains("最大轮次限制"));
    }
}
