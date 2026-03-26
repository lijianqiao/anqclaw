//! Retry wrapper for LLM clients — adds exponential backoff on failure.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::LlmClient;
use crate::types::{ChatMessage, LlmResponse, StreamEvent, ToolDefinition};

pub struct RetryLlmClient {
    inner: Arc<dyn LlmClient>,
    max_retries: u32,
    base_delay: Duration,
}

impl RetryLlmClient {
    pub fn new(inner: Arc<dyn LlmClient>, max_retries: u32, retry_delay_ms: u64) -> Self {
        Self {
            inner,
            max_retries,
            base_delay: Duration::from_millis(retry_delay_ms),
        }
    }
}

impl LlmClient for RetryLlmClient {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
        Box::pin(async move {
            let mut last_error = None;

            for attempt in 0..=self.max_retries {
                match self.inner.chat(messages, tools).await {
                    Ok(response) => return Ok(response),
                    Err(e) => {
                        if attempt < self.max_retries {
                            let delay = self.base_delay * 2u32.pow(attempt);
                            tracing::warn!(
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                delay_ms = delay.as_millis() as u64,
                                error = %e,
                                "LLM call failed, retrying"
                            );
                            tokio::time::sleep(delay).await;
                        }
                        last_error = Some(e);
                    }
                }
            }

            Err(last_error.expect("retry loop must produce at least one error"))
        })
    }

    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<tokio::sync::mpsc::Receiver<StreamEvent>>> + Send + 'a>> {
        // Streaming doesn't retry — the user can retry manually.
        Box::pin(async move { self.inner.chat_stream(messages, tools).await })
    }
}
