//! CLI Channel — stdin/stdout interactive or single-shot mode.
//!
//! Used by `anqclaw chat` to converse without Feishu.

use anyhow::Result;
use std::future::Future;
use std::io::{self, BufRead, Write};
use std::pin::Pin;
use tokio::sync::mpsc;

use crate::types::{InboundMessage, MessageContent, OutboundMessage};

use super::Channel;

// ─── CliChannel ─────────────────────────────────────────────────────────────

pub struct CliChannel {
    /// If `Some`, run in single-shot mode (process this message and exit).
    /// If `None`, run in interactive REPL mode.
    initial_message: Option<String>,
}

impl CliChannel {
    pub fn new(initial_message: Option<String>) -> Self {
        Self { initial_message }
    }
}

impl Channel for CliChannel {
    fn start(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let chat_id = "__cli__".to_string();

            if let Some(ref msg) = self.initial_message {
                // Single-shot mode
                let inbound = InboundMessage {
                    channel: "cli".into(),
                    chat_id,
                    sender_id: "user".into(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    content: MessageContent::Text(msg.clone()),
                    timestamp: chrono::Utc::now().timestamp(),
                    trace_id: String::new(),
                    images: vec![],
                };
                tx.send(inbound).await.ok();
                // Wait a moment for processing, then the main loop will detect shutdown
                // by checking the channel count (single-shot exits after first reply)
                return Ok(());
            }

            // Interactive REPL mode — read from stdin in a blocking thread
            let tx_clone = tx.clone();
            let chat_id_clone = chat_id.clone();
            tokio::task::spawn_blocking(move || {
                let stdin = io::stdin();
                let mut reader = stdin.lock();

                loop {
                    print!("\x1b[36m你: \x1b[0m");
                    io::stdout().flush().ok();

                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break, // EOF
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("stdin error / 标准输入错误: {e}");
                            break;
                        }
                    }

                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if trimmed == "/exit" || trimmed == "/quit" {
                        break;
                    }

                    let inbound = InboundMessage {
                        channel: "cli".into(),
                        chat_id: chat_id_clone.clone(),
                        sender_id: "user".into(),
                        message_id: uuid::Uuid::new_v4().to_string(),
                        content: MessageContent::Text(trimmed.to_string()),
                        timestamp: chrono::Utc::now().timestamp(),
                        trace_id: String::new(),
                        images: vec![],
                    };

                    if tx_clone.blocking_send(inbound).is_err() {
                        break; // receiver dropped
                    }
                }
            })
            .await?;

            Ok(())
        })
    }

    fn send_message(
        &self,
        msg: OutboundMessage,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Print the assistant reply to stdout
            println!("\x1b[32m🤖 anqclaw:\x1b[0m {}", msg.content);
            println!();
            Ok(())
        })
    }

    fn name(&self) -> &str {
        "cli"
    }
}
