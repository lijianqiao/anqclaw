pub mod anthropic;
pub mod openai_compat;
pub mod retry;

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::LlmSection;
use crate::types::{ChatMessage, LlmResponse, StreamEvent, ToolDefinition};

// ─── LlmClient Trait ─────────────────────────────────────────────────────────

/// Unified interface for all LLM providers.
///
/// Object-safe: uses `Pin<Box<dyn Future>>` instead of `async fn` so that
/// `Arc<dyn LlmClient>` works in AgentCore.
pub trait LlmClient: Send + Sync {
    /// Sends a chat completion request.
    ///
    /// - `messages` — the conversation history (system, user, assistant, tool).
    /// - `tools`    — tool definitions the model may call (empty slice = no tools).
    ///
    /// Returns an `LlmResponse` that may contain text, tool calls, or both.
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>>;

    /// Streaming chat completion. Returns a receiver that yields `StreamEvent`s.
    ///
    /// Default implementation wraps `chat()` — emits a single Delta + Done.
    /// Providers can override with real SSE streaming.
    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<tokio::sync::mpsc::Receiver<StreamEvent>>> + Send + 'a>>
    {
        let chat_fut = self.chat(messages, tools);
        Box::pin(async move {
            let resp = chat_fut.await?;
            let (tx, rx) = tokio::sync::mpsc::channel(2);
            if let Some(ref text) = resp.text {
                let _ = tx.send(StreamEvent::Delta(text.clone())).await;
            }
            let _ = tx.send(StreamEvent::Done(resp)).await;
            Ok(rx)
        })
    }
}

// ─── Factory ─────────────────────────────────────────────────────────────────

/// Creates the appropriate LLM client based on the `provider` field in a profile.
///
/// Supported providers:
/// - `"anthropic"` → Anthropic Messages API (Claude)
/// - `"openai_compat"` | `"openai"` | `"deepseek"` | `"qwen"` | `"ollama"` | `"gemini"`
///   → OpenAI-compatible endpoint
///
/// Convenience aliases: `openai`, `deepseek`, `qwen`, `ollama`, `gemini` all resolve
/// to `OpenAiCompatClient` — the only difference is `base_url` + `api_key` in config.
pub fn create_llm_client(config: &LlmSection) -> Result<Arc<dyn LlmClient>> {
    let inner: Arc<dyn LlmClient> = match config.provider.as_str() {
        "anthropic" => Arc::new(anthropic::AnthropicClient::new(config)?),
        "openai_compat" | "openai" | "deepseek" | "qwen" | "ollama" | "gemini" | "mimo" => {
            Arc::new(openai_compat::OpenAiCompatClient::new(config)?)
        }
        other => anyhow::bail!(
            "Unknown LLM provider: `{other}`. Supported: anthropic, openai_compat, openai, \
             deepseek, qwen, ollama, gemini, mimo"
        ),
    };

    // Wrap with retry logic if max_retries > 0
    if config.max_retries > 0 {
        Ok(Arc::new(retry::RetryLlmClient::new(
            inner,
            config.max_retries,
            config.retry_delay_ms,
        )))
    } else {
        Ok(inner)
    }
}
