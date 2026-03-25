//! Heartbeat — periodic task that reads HEARTBEAT.md and runs through the agent pipeline.
//!
//! Flow: tick → read HEARTBEAT.md → build InboundMessage → Agent → Memory → notify (if needed)
//!
//! Convention: If the LLM reply contains "HEARTBEAT_OK", the heartbeat is considered
//! healthy and no notification is sent to the user.
//!
//! TODO(future): When splitting into workspace crates, extract into `crates/heartbeat/`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::config::HeartbeatSection;
use crate::memory::MemoryStore;
use crate::types::InboundMessage;

// ─── Heartbeat ──────────────────────────────────────────────────────────────

pub struct Heartbeat {
    interval: Duration,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    channels: Vec<Arc<dyn Channel>>,
    prompt_path: PathBuf,
    notify_chat_id: String,
    notify_channel: String,
}

impl Heartbeat {
    pub fn new(
        config: &HeartbeatSection,
        agent: Arc<AgentCore>,
        memory: Arc<MemoryStore>,
        channels: Vec<Arc<dyn Channel>>,
        workspace_path: &str,
    ) -> Self {
        Self {
            interval: Duration::from_secs(config.interval_minutes as u64 * 60),
            agent,
            memory,
            channels,
            prompt_path: PathBuf::from(workspace_path).join("HEARTBEAT.md"),
            notify_chat_id: config.notify_chat_id.clone(),
            notify_channel: config.notify_channel.clone(),
        }
    }

    /// Runs the heartbeat loop forever.
    ///
    /// Each tick: read HEARTBEAT.md → agent.handle → save conversation →
    /// notify user (unless reply contains "HEARTBEAT_OK").
    pub async fn run(&self) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(self.interval);

        // Consume the immediate first tick so we don't fire at t=0
        interval.tick().await;

        tracing::info!(
            interval_mins = self.interval.as_secs() / 60,
            "heartbeat: started"
        );

        loop {
            interval.tick().await;

            // Re-read prompt each tick — changes take effect without restart
            let prompt = match tokio::fs::read_to_string(&self.prompt_path).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "heartbeat: HEARTBEAT.md not found, skipping");
                    continue;
                }
            };

            if prompt.trim().is_empty() {
                continue;
            }

            tracing::info!("heartbeat: tick — running agent");

            let msg = InboundMessage::heartbeat(&prompt);

            // Load heartbeat-specific history (separate chat_id "__heartbeat__")
            let history = self
                .memory
                .get_history(&msg.chat_id, 5)
                .await
                .unwrap_or_default();

            let (mut reply, conversation) = self.agent.handle(&msg, &history).await;

            // Persist heartbeat conversation history
            if !conversation.is_empty()
                && let Err(e) = self
                    .memory
                    .save_conversation(&msg.chat_id, &conversation)
                    .await
            {
                tracing::error!(error = %e, "heartbeat: failed to save conversation");
            }

            // "HEARTBEAT_OK" convention — LLM says everything is fine, skip notification
            if reply.content.contains("HEARTBEAT_OK") {
                tracing::debug!("heartbeat: HEARTBEAT_OK — no notification needed");
                continue;
            }

            // Route notification to configured channel/chat
            reply.chat_id = self.notify_chat_id.clone();
            reply.channel = self.notify_channel.clone();

            if let Some(ch) = self.channels.iter().find(|c| c.name() == self.notify_channel) {
                if let Err(e) = ch.send_message(reply).await {
                    tracing::error!(
                        channel = self.notify_channel.as_str(),
                        error = %e,
                        "heartbeat: failed to send notification"
                    );
                }
            } else {
                tracing::warn!(
                    channel = self.notify_channel.as_str(),
                    "heartbeat: no matching channel for notification"
                );
            }
        }
    }
}
