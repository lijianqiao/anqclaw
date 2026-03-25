//! Agent Core — agentic loop with LLM ↔ tool calling.
//!
//! TODO(future): When splitting into workspace crates, extract into
//! `crates/agent/` with its own `Cargo.toml`.

pub mod context;
pub mod prompt;

use std::sync::Arc;

use anyhow::Result;

use crate::config::AppConfig;
use crate::llm::LlmClient;
use crate::memory::MemoryStore;
use crate::tool::ToolRegistry;
use crate::types::{ChatMessage, InboundMessage, OutboundMessage};

use context::{build_system_prompt, format_memories};

// ─── AgentCore ───────────────────────────────────────────────────────────────

pub struct AgentCore {
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
}

impl AgentCore {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        tools: Arc<ToolRegistry>,
        memory: Arc<MemoryStore>,
        config: Arc<AppConfig>,
    ) -> Self {
        Self {
            llm,
            tools,
            memory,
            config,
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
            let response = self.llm.chat(&messages, &tool_defs).await?;

            let has_tool_calls = !response.tool_calls.is_empty();
            let has_text = response.text.is_some();

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

                // Execute all tool calls concurrently
                let results = self.tools.execute_batch(&response.tool_calls).await;

                // Append each tool result
                for result in &results {
                    messages.push(ChatMessage::tool_result(result));
                }

                // Continue loop — let LLM see the results
            } else if has_text {
                // Pure text response — done
                let text = response.text.unwrap();
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

        let tools = Arc::new(ToolRegistry::new(&config.tools, memory.clone()));
        let agent = AgentCore::new(mock_llm, tools, memory, config);

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

        let tools = Arc::new(ToolRegistry::new(&config.tools, memory.clone()));
        let agent = AgentCore::new(mock_llm, tools, memory, config);

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

        let tools = Arc::new(ToolRegistry::new(&config.tools, memory.clone()));
        let agent = AgentCore::new(mock_llm, tools, memory, config);

        let (reply, _persist) = agent.handle(&test_inbound(), &[]).await;

        assert!(reply.content.contains("最大轮次限制"));
    }
}
