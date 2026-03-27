//! Gateway — routes inbound messages from channels through the agent pipeline.
//!
//! Flow: Channel → mpsc → Gateway main loop → Agent → Memory → Channel reply
//!
//! TODO(future): When splitting into workspace crates, extract into `crates/gateway/`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use lru::LruCache;
use std::num::NonZero;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::config::AppConfig;
use crate::memory::MemoryStore;
use crate::metrics::Metrics;
use crate::types::{InboundMessage, OutboundMessage};

// ─── Gateway ─────────────────────────────────────────────────────────────────

pub struct Gateway {
    /// Channels indexed by name for O(1) lookup.
    channels: HashMap<String, Arc<dyn Channel>>,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
    metrics: Arc<Metrics>,
    /// Per-chat mutex to serialise processing within the same conversation.
    chat_locks: DashMap<String, Arc<Mutex<()>>>,
    /// Recent message IDs for dedup (LRU, capacity 1000).
    recent_ids: Mutex<LruCache<String, ()>>,
    /// Per-chat sliding-window rate limiter: chat_id → list of request timestamps.
    rate_limiter: DashMap<String, Vec<Instant>>,
}

impl Gateway {
    pub fn new(
        channels: Vec<Arc<dyn Channel>>,
        agent: Arc<AgentCore>,
        memory: Arc<MemoryStore>,
        config: Arc<AppConfig>,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        let channel_map: HashMap<String, Arc<dyn Channel>> = channels
            .into_iter()
            .map(|ch| (ch.name().to_string(), ch))
            .collect();
        Arc::new(Self {
            channels: channel_map,
            agent,
            memory,
            config,
            metrics,
            chat_locks: DashMap::new(),
            recent_ids: Mutex::new(LruCache::new(NonZero::new(1000).expect("1000 is non-zero"))),
            rate_limiter: DashMap::new(),
        })
    }

    /// Starts all channels and the main message processing loop.
    ///
    /// Returns when all channels have closed AND all in-flight messages have
    /// been processed (no orphaned tasks).
    pub async fn run(self: &Arc<Self>) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<InboundMessage>(256);

        // Spawn all channel listeners
        for ch in self.channels.values() {
            let tx = tx.clone();
            let ch = ch.clone();
            tokio::spawn(async move {
                if let Err(e) = ch.start(tx).await {
                    tracing::error!(channel = ch.name(), error = %e, "channel exited with error");
                }
            });
        }

        // Monitor queue depth periodically
        let tx_monitor = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                let capacity = tx_monitor.capacity();
                // Channel max capacity is 256; warn when less than 25% remains
                if capacity < 64 {
                    tracing::warn!(
                        remaining_capacity = capacity,
                        "gateway: message queue pressure — capacity below 25%"
                    );
                }
                if tx_monitor.is_closed() {
                    break;
                }
            }
        });

        // Periodic GC for chat_locks and rate_limiter (every 60s instead of per-message)
        let gw_gc = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                // Remove chat locks with no active holders
                gw_gc.chat_locks.retain(|_, v| Arc::strong_count(v) > 1);
                // Remove empty rate limiter entries (stale sessions)
                gw_gc.rate_limiter.retain(|_, timestamps| !timestamps.is_empty());
            }
        });

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

    /// Compute the session key for history lookup/storage based on config strategy.
    /// - "chat" (default): use chat_id only (group chats share history)
    /// - "user": use sender_id only (each user has own global history)
    /// - "chat_user": use "chat_id::sender_id" (per-user history within each chat)
    fn session_key(&self, msg: &InboundMessage) -> String {
        match self.config.memory.session_key_strategy.as_str() {
            "user" => msg.sender_id.clone(),
            "chat_user" => format!("{}::{}", msg.chat_id, msg.sender_id),
            _ => msg.chat_id.clone(), // "chat" is default
        }
    }

    /// O(1) channel lookup by name.
    fn find_channel(&self, name: &str) -> Option<&Arc<dyn Channel>> {
        self.channels.get(name)
    }

    async fn process_message(&self, msg: InboundMessage) {
        // Assign trace_id if not already set
        let mut msg = msg;
        if msg.trace_id.is_empty() {
            msg.trace_id = uuid::Uuid::new_v4().to_string();
        }

        // Metrics: count request
        self.metrics
            .total_requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let session_key = self.session_key(&msg);

        // Rate limiting: sliding window per session_key
        let max_rpm = self.config.limits.max_requests_per_minute;
        if max_rpm > 0 {
            let now = Instant::now();
            let window = std::time::Duration::from_secs(60);
            let mut entry = self.rate_limiter.entry(session_key.clone()).or_default();
            // Prune timestamps outside the window
            entry.retain(|t| now.duration_since(*t) < window);
            if entry.len() >= max_rpm as usize {
                tracing::warn!(
                    session_key = %session_key,
                    limit = max_rpm,
                    "rate limit exceeded"
                );
                let reply = OutboundMessage::error(&msg, "请求过于频繁，请稍后再试");
                if let Some(ch) = self.find_channel(&msg.channel) {
                    let _ = ch.send_message(reply).await;
                }
                return;
            }
            entry.push(now);
            // No per-request GC — idle entries are bounded by max_rpm timestamps
            // and self-clean on next access via the retain() above.
        }

        // Message length validation
        let max_len = self.config.limits.max_message_length as usize;
        if max_len > 0 {
            let text = msg.content.to_text();
            if text.len() > max_len {
                tracing::warn!(
                    session_key = %session_key,
                    len = text.len(),
                    max = max_len,
                    "message too long, rejecting"
                );
                let reply = OutboundMessage::error(
                    &msg,
                    &format!("消息过长（{} 字符），最大允许 {} 字符", text.len(), max_len),
                );
                if let Some(ch) = self.find_channel(&msg.channel) {
                    let _ = ch.send_message(reply).await;
                }
                return;
            }
        }

        // Per-chat lock: serialise processing within the same conversation
        let lock = self
            .chat_locks
            .entry(session_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // 0. Acknowledge receipt (fire-and-forget reaction)
        if let Some(ch) = self.find_channel(&msg.channel)
            && let Err(e) = ch.acknowledge(&msg).await
        {
            tracing::debug!(error = %e, "acknowledge failed (non-critical)");
        }

        // 1. Load history from SQLite
        let history = self
            .memory
            .get_history(&session_key, self.config.memory.history_limit as usize)
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
                .save_conversation(&session_key, &persist_messages)
                .await
        {
            tracing::error!(error = %e, "failed to save conversation");
        }

        // 4. Send reply through the originating channel
        if let Some(ch) = self.find_channel(&msg.channel) {
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

        drop(_guard);
    }
}
