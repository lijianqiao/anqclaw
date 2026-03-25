//! Gateway — routes inbound messages from channels through the agent pipeline.
//!
//! Flow: Channel → mpsc → Gateway main loop → Agent → Memory → Channel reply
//!
//! TODO(future): When splitting into workspace crates, extract into `crates/gateway/`.

use std::sync::Arc;

use dashmap::DashMap;
use lru::LruCache;
use std::num::NonZero;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinSet;

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::config::AppConfig;
use crate::memory::MemoryStore;
use crate::types::InboundMessage;

// ─── Gateway ─────────────────────────────────────────────────────────────────

pub struct Gateway {
    channels: Vec<Arc<dyn Channel>>,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
    /// Per-chat mutex to serialise processing within the same conversation.
    chat_locks: DashMap<String, Arc<Mutex<()>>>,
    /// Recent message IDs for dedup (LRU, capacity 1000).
    recent_ids: Mutex<LruCache<String, ()>>,
}

impl Gateway {
    pub fn new(
        channels: Vec<Arc<dyn Channel>>,
        agent: Arc<AgentCore>,
        memory: Arc<MemoryStore>,
        config: Arc<AppConfig>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channels,
            agent,
            memory,
            config,
            chat_locks: DashMap::new(),
            recent_ids: Mutex::new(LruCache::new(NonZero::new(1000).unwrap())),
        })
    }

    /// Starts all channels and the main message processing loop.
    ///
    /// Returns when all channels have closed AND all in-flight messages have
    /// been processed (no orphaned tasks).
    pub async fn run(self: &Arc<Self>) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<InboundMessage>(256);

        // Spawn all channel listeners
        for ch in &self.channels {
            let tx = tx.clone();
            let ch = ch.clone();
            tokio::spawn(async move {
                if let Err(e) = ch.start(tx).await {
                    tracing::error!(channel = ch.name(), error = %e, "channel exited with error");
                }
            });
        }

        // Drop our copy so rx closes when all channel senders are gone
        drop(tx);

        tracing::info!("gateway: message loop started");

        // Track all spawned process_message tasks
        let mut tasks = JoinSet::new();

        // Main message loop
        while let Some(msg) = rx.recv().await {
            // Dedup by message_id
            if !msg.message_id.is_empty() {
                let mut recent = self.recent_ids.lock().await;
                if recent.get(&msg.message_id).is_some() {
                    tracing::debug!(msg_id = %msg.message_id, "gateway: duplicate, skipping");
                    continue;
                }
                recent.put(msg.message_id.clone(), ());
            }

            // Spawn a task for each message, tracked by JoinSet
            let gw = self.clone();
            tasks.spawn(async move {
                gw.process_message(msg).await;
            });
        }

        // Wait for ALL in-flight message tasks to complete before returning.
        // This prevents the caller from closing DB / exiting while tasks are active.
        tracing::info!(
            pending = tasks.len(),
            "gateway: message loop ended, waiting for in-flight tasks"
        );
        while tasks.join_next().await.is_some() {}

        tracing::info!("gateway: all tasks completed");
        Ok(())
    }

    async fn process_message(&self, msg: InboundMessage) {
        // Per-chat lock: serialise processing within the same conversation
        let lock = self
            .chat_locks
            .entry(msg.chat_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // 1. Load history from SQLite
        let history = self
            .memory
            .get_history(&msg.chat_id, self.config.memory.history_limit as usize)
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "failed to load history");
                vec![]
            });

        // 2. Agent processes the message
        let (reply, persist_messages) = self.agent.handle(&msg, &history).await;

        // 3. Persist new messages to SQLite
        if !persist_messages.is_empty()
            && let Err(e) = self
                .memory
                .save_conversation(&msg.chat_id, &persist_messages)
                .await
        {
            tracing::error!(error = %e, "failed to save conversation");
        }

        // 4. Send reply through the originating channel
        if let Some(ch) = self.channels.iter().find(|c| c.name() == msg.channel) {
            if let Err(e) = ch.send_message(reply).await {
                tracing::error!(
                    channel = ch.name(),
                    error = %e,
                    "failed to send reply"
                );
            }
        } else {
            tracing::warn!(channel = %msg.channel, "no matching channel for reply");
        }
    }
}
