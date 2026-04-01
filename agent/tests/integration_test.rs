//! Integration tests — Agent + Memory + Tools end-to-end.
//!
//! Uses a mock LlmClient to test the complete chain:
//! InboundMessage → AgentCore → ToolRegistry → MemoryStore → OutboundMessage

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;

use anqclaw::agent::AgentCore;
use anqclaw::config::AppConfig;
use anqclaw::llm::LlmClient;
use anqclaw::memory::MemoryStore;
use anqclaw::skill::{SkillRegistry, SkillSource};
use anqclaw::tool::ToolRegistry;
use anqclaw::types::*;

// ─── Mock LLM Client ────────────────────────────────────────────────────────

struct MockLlm {
    responses: Vec<LlmResponse>,
    call_count: AtomicU32,
    last_messages: Mutex<Vec<ChatMessage>>,
}

impl MockLlm {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses,
            call_count: AtomicU32::new(0),
            last_messages: Mutex::new(Vec::new()),
        }
    }

    fn last_messages(&self) -> Vec<ChatMessage> {
        self.last_messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl LlmClient for MockLlm {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        _tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
        Box::pin(async {
            *self.last_messages.lock().unwrap_or_else(|e| e.into_inner()) = messages.to_vec();
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            if idx < self.responses.len() {
                Ok(self.responses[idx].clone())
            } else {
                Ok(self.responses.last().unwrap().clone())
            }
        })
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn test_config() -> Arc<AppConfig> {
    let toml_str = r#"
[app]
name = "integration-test"
workspace = "./test_workspace_nonexistent"
log_level = "debug"

[feishu]
app_id = "test_app_id"
app_secret = "test_secret"

[llm]
provider = "anthropic"
model = "test-model"
api_key = "test_key"

[tools]
shell_enabled = true
shell_allowed_commands = ["echo", "date"]
file_enabled = false
web_fetch_enabled = false
memory_tool_enabled = true

[agent]
max_tool_rounds = 5
"#;
    Arc::new(AppConfig::load_from_str(toml_str).unwrap())
}

fn test_inbound(text: &str) -> InboundMessage {
    InboundMessage {
        channel: "test".into(),
        chat_id: "chat_integration".into(),
        sender_id: "user_test".into(),
        message_id: format!("msg_{}", uuid::Uuid::new_v4()),
        content: MessageContent::Text(text.into()),
        timestamp: chrono::Utc::now().timestamp(),
        trace_id: String::new(),
        images: vec![],
    }
}

fn test_config_with_file_access(workspace: &std::path::Path) -> Arc<AppConfig> {
    let workspace = workspace.to_string_lossy().replace('\\', "/");
    let toml_str = format!(
        r#"
[app]
name = "integration-test"
workspace = "{workspace}"
log_level = "debug"

[feishu]
app_id = "test_app_id"
app_secret = "test_secret"

[llm]
provider = "anthropic"
model = "test-model"
api_key = "test_key"

[tools]
shell_enabled = false
file_enabled = true
file_access_dir = "{workspace}"
web_fetch_enabled = false
memory_tool_enabled = false

[agent]
max_tool_rounds = 5
"#
    );
    Arc::new(AppConfig::load_from_str(&toml_str).unwrap())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Test: pure text reply → no tool calls
#[tokio::test]
async fn test_pure_text_reply() {
    let config = test_config();
    let memory = Arc::new(MemoryStore::new(":memory:").await.unwrap());

    let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
        text: Some("Hello! How can I help?".into()),
        tool_calls: vec![],
    }]));

    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        None,
        vec![],
        None,
    ));
    let agent = AgentCore::new(mock_llm, None, tools, memory.clone(), config, None, None).await;

    let msg = test_inbound("Hi");
    let (reply, persist) = agent.handle(&msg, &[]).await;

    assert_eq!(reply.content, "Hello! How can I help?");
    assert_eq!(reply.channel, "test");
    assert_eq!(reply.chat_id, "chat_integration");
    assert_eq!(reply.reply_to.as_deref(), Some(msg.message_id.as_str()));
    // persist: user message + assistant message
    assert_eq!(persist.len(), 2);
}

/// Test: tool calling loop → LLM requests tool, sees result, replies
#[tokio::test]
async fn test_tool_call_and_reply() {
    let config = test_config();
    let memory = Arc::new(MemoryStore::new(":memory:").await.unwrap());

    let mock_llm = Arc::new(MockLlm::new(vec![
        // Round 1: LLM wants to save a memory
        LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_save".into(),
                name: "memory_save".into(),
                arguments: serde_json::json!({
                    "key": "user_name",
                    "content": "The user's name is Test User",
                    "tags": "user,name"
                }),
            }],
        },
        // Round 2: LLM sees result, replies with text
        LlmResponse {
            text: Some("I've saved your name!".into()),
            tool_calls: vec![],
        },
    ]));

    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        None,
        vec![],
        None,
    ));
    let agent = AgentCore::new(mock_llm, None, tools, memory.clone(), config, None, None).await;

    let msg = test_inbound("My name is Test User");
    let (reply, persist) = agent.handle(&msg, &[]).await;

    assert_eq!(reply.content, "I've saved your name!");
    assert_eq!(reply.reply_to.as_deref(), Some(msg.message_id.as_str()));
    // persist: user + assistant(tool_call) + tool_result + assistant(text)
    assert_eq!(persist.len(), 4);

    // Verify memory was actually saved
    let memories = memory.search_memory("user_name", 5).await.unwrap();
    assert!(!memories.is_empty());
    assert!(memories[0].content.contains("Test User"));
}

/// Test: conversation history persistence round-trip
#[tokio::test]
async fn test_history_persistence() {
    let config = test_config();
    let memory = Arc::new(MemoryStore::new(":memory:").await.unwrap());

    let mock_llm = Arc::new(MockLlm::new(vec![
        LlmResponse {
            text: Some("First reply".into()),
            tool_calls: vec![],
        },
        LlmResponse {
            text: Some("Second reply with context".into()),
            tool_calls: vec![],
        },
    ]));

    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        None,
        vec![],
        None,
    ));
    let agent = AgentCore::new(
        mock_llm,
        None,
        tools,
        memory.clone(),
        config.clone(),
        None,
        None,
    )
    .await;

    // First message
    let msg1 = test_inbound("Hello");
    let (_reply1, persist1) = agent.handle(&msg1, &[]).await;

    // Save conversation to SQLite
    memory
        .save_conversation(&msg1.chat_id, &persist1)
        .await
        .unwrap();

    // Load history for second message
    let history = memory
        .get_history(&msg1.chat_id, config.memory.history_limit as usize)
        .await
        .unwrap();

    assert!(!history.is_empty());

    // Second message with history
    let msg2 = test_inbound("Follow up");
    let (reply2, _persist2) = agent.handle(&msg2, &history).await;

    assert_eq!(reply2.content, "Second reply with context");
}

/// Test: multi-tool sequential calls
#[tokio::test]
async fn test_multi_tool_calls() {
    let config = test_config();
    let memory = Arc::new(MemoryStore::new(":memory:").await.unwrap());

    let mock_llm = Arc::new(MockLlm::new(vec![
        // Round 1: Two tool calls at once
        LlmResponse {
            text: Some("Let me save and search.".into()),
            tool_calls: vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "memory_save".into(),
                    arguments: serde_json::json!({
                        "key": "fact_1",
                        "content": "Important fact for testing"
                    }),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "memory_search".into(),
                    arguments: serde_json::json!({
                        "query": "testing"
                    }),
                },
            ],
        },
        // Round 2: Reply
        LlmResponse {
            text: Some("Done with both operations!".into()),
            tool_calls: vec![],
        },
    ]));

    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        None,
        vec![],
        None,
    ));
    let agent = AgentCore::new(mock_llm, None, tools, memory.clone(), config, None, None).await;

    let msg = test_inbound("Save and search");
    let (reply, persist) = agent.handle(&msg, &[]).await;

    assert_eq!(reply.content, "Done with both operations!");
    // persist: user + assistant(2 tool_calls) + tool_result_1 + tool_result_2 + assistant(text)
    assert_eq!(persist.len(), 5);
}

/// Test: max rounds exceeded
#[tokio::test]
async fn test_max_rounds_guard() {
    let toml_str = r#"
[app]
name = "test"
workspace = "./test_workspace_nonexistent"

[feishu]
app_id = "test"
app_secret = "test"

[llm]
provider = "anthropic"
model = "test"
api_key = "test"

[agent]
max_tool_rounds = 2
"#;
    let config = Arc::new(AppConfig::load_from_str(toml_str).unwrap());
    let memory = Arc::new(MemoryStore::new(":memory:").await.unwrap());

    // Always returns tool calls → never text
    let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
        text: None,
        tool_calls: vec![ToolCall {
            id: "call_loop".into(),
            name: "memory_search".into(),
            arguments: serde_json::json!({"query": "test"}),
        }],
    }]));

    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        None,
        vec![],
        None,
    ));
    let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

    let msg = test_inbound("trigger loop");
    let (reply, _) = agent.handle(&msg, &[]).await;

    assert!(reply.content.contains("最大轮次限制"));
}

#[tokio::test]
async fn test_skill_summary_path_can_be_read_via_file_read() {
    let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_summary_workspace");
    let skill_root = std::env::temp_dir().join("anqclaw_test_skill_summary_user_skills");
    let _ = std::fs::remove_dir_all(&workspace_dir);
    let _ = std::fs::remove_dir_all(&skill_root);
    std::fs::create_dir_all(&workspace_dir).unwrap();
    std::fs::create_dir_all(skill_root.join("xlsx")).unwrap();

    let skill_path = skill_root.join("xlsx").join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: xlsx\ndescription: Spreadsheet skill\nextensions:\n  - .xlsx\n---\nUse pandas for spreadsheet inspection.",
    )
    .unwrap();

    let config = test_config_with_file_access(&workspace_dir);
    let memory = Arc::new(MemoryStore::new(":memory:").await.unwrap());
    let skill_registry = Arc::new(SkillRegistry::scan(
        vec![SkillSource::new("user", skill_root.clone())],
        256 * 1024,
    ));
    let skill_path_for_tool = skill_path.to_string_lossy().replace('\\', "/");

    let mock_llm = Arc::new(MockLlm::new(vec![
        LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_read_skill".into(),
                name: "file_read".into(),
                arguments: serde_json::json!({
                    "path": skill_path_for_tool,
                }),
            }],
        },
        LlmResponse {
            text: Some("Skill loaded via file_read.".into()),
            tool_calls: vec![],
        },
    ]));

    let tools = Arc::new(ToolRegistry::new(
        &config.tools,
        &config.security,
        &config.agent,
        memory.clone(),
        Some(skill_registry.clone()),
        vec![],
        Some(&config.skills),
    ));
    let agent = AgentCore::new(
        mock_llm.clone(),
        None,
        tools,
        memory,
        config,
        None,
        Some(skill_registry.clone()),
    )
    .await;

    let msg = test_inbound("请查看这个 xlsx skill 怎么做表格分析");
    let (reply, persist) = agent.handle(&msg, &[]).await;

    assert_eq!(reply.content, "Skill loaded via file_read.");
    assert!(persist.iter().any(|message| {
        message.role == Role::Tool
            && message
                .content
                .contains("Use pandas for spreadsheet inspection.")
    }));

    let llm_messages = mock_llm.last_messages();
    let expected_location = skill_registry.find("xlsx").unwrap().prompt_location();
    assert!(llm_messages.iter().any(|message| {
        message.role == Role::System
            && message.content.contains("<available_skills>")
            && message.content.contains("<name>xlsx</name>")
            && message.content.contains(&expected_location)
    }));

    let _ = std::fs::remove_dir_all(&workspace_dir);
    let _ = std::fs::remove_dir_all(&skill_root);
}
