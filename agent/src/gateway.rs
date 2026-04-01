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
use tokio_util::sync::CancellationToken;

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::config::AppConfig;
use crate::memory::MemoryStore;
use crate::metrics::Metrics;
use crate::session::build_session_key;
use crate::types::{InboundMessage, OutboundMessage};

const RECENT_MESSAGE_CACHE_CAPACITY: usize = 1000;
const GATEWAY_MESSAGE_QUEUE_CAPACITY: usize = 256;
const GATEWAY_QUEUE_MONITOR_INTERVAL_SECS: u64 = 10;
const GATEWAY_QUEUE_PRESSURE_MIN_REMAINING_CAPACITY: usize = 64;
const GATEWAY_GC_INTERVAL_SECS: u64 = 60;
const GATEWAY_RATE_LIMIT_WINDOW_SECS: u64 = 60;
const PREVALIDATED_REQUEST_TTL_SECS: u64 = 300;
const RATE_LIMITER_MAX_ENTRIES: usize = 10_000;

fn evict_oldest_rate_limiter_entries(
    rate_limiter: &DashMap<String, Vec<Instant>>,
    max_entries: usize,
) -> usize {
    let overflow = rate_limiter.len().saturating_sub(max_entries);
    if overflow == 0 {
        return 0;
    }

    let mut eviction_candidates: Vec<(String, Instant)> = rate_limiter
        .iter()
        .map(|entry| {
            let last_seen = entry.value().last().copied().unwrap_or_else(Instant::now);
            (entry.key().clone(), last_seen)
        })
        .collect();
    eviction_candidates.sort_by_key(|(_, last_seen)| *last_seen);

    let mut evicted = 0;
    for (session_key, _) in eviction_candidates.into_iter().take(overflow) {
        if rate_limiter.remove(&session_key).is_some() {
            evicted += 1;
        }
    }

    evicted
}

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
    /// Recent message IDs for dedup.
    recent_ids: Mutex<LruCache<String, ()>>,
    /// Per-chat sliding-window rate limiter: chat_id → list of request timestamps.
    rate_limiter: DashMap<String, Vec<Instant>>,
    /// HTTP requests prevalidated before entering the gateway queue.
    prevalidated_requests: DashMap<String, Instant>,
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
            recent_ids: Mutex::new(LruCache::new(
                NonZero::new(RECENT_MESSAGE_CACHE_CAPACITY)
                    .expect("message cache capacity must be non-zero"),
            )),
            rate_limiter: DashMap::new(),
            prevalidated_requests: DashMap::new(),
        })
    }

    /// Starts all channels and the main message processing loop.
    ///
    /// Returns when all channels have closed AND all in-flight messages have
    /// been processed (no orphaned tasks).
    pub async fn run(self: &Arc<Self>, shutdown: CancellationToken) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<InboundMessage>(GATEWAY_MESSAGE_QUEUE_CAPACITY);

        // Spawn all channel listeners
        for ch in self.channels.values() {
            let tx = tx.clone();
            let ch = ch.clone();
            tokio::spawn(async move {
                if let Err(e) = ch.start(tx).await {
                    tracing::error!(channel = ch.name(), error = %e, "channel exited with error / 频道退出并出错");
                }
            });
        }

        // Monitor queue depth periodically
        let tx_monitor = tx.clone();
        let shutdown_monitor = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                GATEWAY_QUEUE_MONITOR_INTERVAL_SECS,
            ));
            loop {
                tokio::select! {
                    _ = shutdown_monitor.cancelled() => break,
                    _ = interval.tick() => {}
                }
                let capacity = tx_monitor.capacity();
                if capacity < GATEWAY_QUEUE_PRESSURE_MIN_REMAINING_CAPACITY {
                    tracing::warn!(
                        remaining_capacity = capacity,
                        "gateway: message queue pressure — capacity below 25% / 网关: 消息队列压力 - 容量低于 25%"
                    );
                }
                if tx_monitor.is_closed() {
                    break;
                }
            }
        });

        // Periodic GC for chat_locks and rate_limiter (every 60s instead of per-message)
        let gw_gc = self.clone();
        let shutdown_gc = shutdown.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(GATEWAY_GC_INTERVAL_SECS));
            loop {
                tokio::select! {
                    _ = shutdown_gc.cancelled() => break,
                    _ = interval.tick() => {}
                }
                // Remove chat locks with no active holders
                gw_gc.chat_locks.retain(|_, v| Arc::strong_count(v) > 1);
                // Remove empty rate limiter entries (stale sessions)
                gw_gc
                    .rate_limiter
                    .retain(|_, timestamps| !timestamps.is_empty());
                let now = Instant::now();
                let prevalidated_cutoff = now
                    .checked_sub(std::time::Duration::from_secs(
                        PREVALIDATED_REQUEST_TTL_SECS,
                    ))
                    .unwrap_or(now);
                gw_gc
                    .prevalidated_requests
                    .retain(|_, validated_at| *validated_at >= prevalidated_cutoff);
                // Safety cap: evict the least-recently-used sessions instead of
                // clearing all rate-limit state, which would let new keys reset
                // already-limited sessions.
                let evicted = evict_oldest_rate_limiter_entries(
                    &gw_gc.rate_limiter,
                    RATE_LIMITER_MAX_ENTRIES,
                );
                if evicted > 0 {
                    tracing::warn!(
                        evicted,
                        remaining = gw_gc.rate_limiter.len(),
                        limit = RATE_LIMITER_MAX_ENTRIES,
                        "rate_limiter evicted oldest entries due to excessive sessions / 速率限制器条目过多，已淘汰最久未使用的会话"
                    );
                }
            }
        });

        // Drop our copy so rx closes when all channel senders are gone
        drop(tx);

        tracing::info!("gateway: message loop started / 网关: 消息循环已启动");

        // Track all spawned process_message tasks
        let mut tasks = JoinSet::new();

        // Main message loop — stop accepting new messages on shutdown signal
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    tracing::info!("gateway: shutdown signal received, stopping message intake / 网关: 收到关闭信号，停止接收新消息");
                    break;
                }
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };

                    // Dedup by message_id
                    if !msg.message_id.is_empty() {
                        let mut recent = self.recent_ids.lock().await;
                        if recent.get(&msg.message_id).is_some() {
                            self.forget_prevalidated_message(&msg.message_id);
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
            }
        }

        // Wait for ALL in-flight message tasks to complete before returning.
        // This prevents the caller from closing DB / exiting while tasks are active.
        tracing::info!(
            pending = tasks.len(),
            "gateway: message loop ended, waiting for in-flight tasks / 网关: 消息循环结束，等待进行中的任务"
        );
        while tasks.join_next().await.is_some() {}

        tracing::info!("gateway: all tasks completed / 网关: 所有任务已完成");
        Ok(())
    }

    /// Compute the session key for history lookup/storage based on config strategy.
    /// - "chat" (default): use chat_id only (group chats share history)
    /// - "user": use sender_id only (each user has own global history)
    /// - "chat_user": use "chat_id::sender_id" (per-user history within each chat)
    fn session_key(&self, msg: &InboundMessage) -> String {
        build_session_key(
            self.config.memory.session_key_strategy.as_str(),
            &msg.chat_id,
            &msg.sender_id,
        )
    }

    /// O(1) channel lookup by name.
    fn find_channel(&self, name: &str) -> Option<&Arc<dyn Channel>> {
        self.channels.get(name)
    }

    fn validate_message_length(&self, msg: &InboundMessage) -> Result<(), RequestValidationError> {
        let max_len = self.config.limits.max_message_length as usize;
        if max_len == 0 {
            return Ok(());
        }

        let text = msg.content.to_text();
        if text.len() > max_len {
            return Err(RequestValidationError::MessageTooLong {
                len: text.len(),
                max: max_len,
            });
        }

        Ok(())
    }

    fn acquire_rate_limit_slot(&self, session_key: &str) -> Result<(), RequestValidationError> {
        let max_rpm = self.config.limits.max_requests_per_minute;
        if max_rpm == 0 {
            return Ok(());
        }

        let now = Instant::now();
        let window = std::time::Duration::from_secs(GATEWAY_RATE_LIMIT_WINDOW_SECS);
        let mut entry = self
            .rate_limiter
            .entry(session_key.to_string())
            .or_default();
        entry.retain(|t| now.duration_since(*t) < window);
        if entry.len() >= max_rpm as usize {
            tracing::warn!(
                session_key = %session_key,
                limit = max_rpm,
                "rate limit exceeded / 超出速率限制"
            );
            return Err(RequestValidationError::RateLimited);
        }
        entry.push(now);
        Ok(())
    }

    fn validate_request(
        &self,
        msg: &InboundMessage,
        session_key: &str,
    ) -> Result<(), RequestValidationError> {
        self.acquire_rate_limit_slot(session_key)?;
        self.validate_message_length(msg)
    }

    fn request_error_reply(
        msg: &InboundMessage,
        error: &RequestValidationError,
    ) -> OutboundMessage {
        match error {
            RequestValidationError::RateLimited => {
                OutboundMessage::error(msg, "请求过于频繁，请稍后再试")
            }
            RequestValidationError::MessageTooLong { len, max } => OutboundMessage::error(
                msg,
                &format!("消息过长（{} 字符），最大允许 {} 字符", len, max),
            ),
        }
    }

    fn take_prevalidated_message(&self, message_id: &str) -> bool {
        if message_id.is_empty() {
            return false;
        }

        self.prevalidated_requests.remove(message_id).is_some()
    }

    pub fn prevalidate_http_request(
        &self,
        msg: &InboundMessage,
    ) -> Result<(), RequestValidationError> {
        let session_key = self.session_key(msg);
        self.validate_request(msg, &session_key)?;
        if !msg.message_id.is_empty() {
            self.prevalidated_requests
                .insert(msg.message_id.clone(), Instant::now());
        }
        Ok(())
    }

    pub fn forget_prevalidated_message(&self, message_id: &str) {
        if message_id.is_empty() {
            return;
        }

        self.prevalidated_requests.remove(message_id);
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
        let prevalidated = self.take_prevalidated_message(&msg.message_id);

        if !prevalidated && let Err(error) = self.validate_request(&msg, &session_key) {
            let reply = Self::request_error_reply(&msg, &error);
            if let Some(ch) = self.find_channel(&msg.channel) {
                let _ = ch.send_message(reply).await;
            }
            return;
        }

        // Per-chat lock: serialise processing within the same conversation
        let lock = self
            .chat_locks
            .entry(session_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // Track active sessions for metrics
        self.metrics
            .active_sessions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // 0. Acknowledge receipt (fire-and-forget reaction)
        if let Some(ch) = self.find_channel(&msg.channel)
            && let Err(e) = ch.acknowledge(&msg).await
        {
            tracing::debug!(error = %e, "acknowledge failed (non-critical) / 确认失败（非关键）");
        }

        // 1. Load history from SQLite
        let history = self
            .memory
            .get_history(&session_key, self.config.memory.history_limit as usize)
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "failed to load history / 加载历史记录失败");
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
            tracing::error!(error = %e, "failed to save conversation / 保存对话失败");
        }

        // 4. Send reply through the originating channel
        if let Some(ch) = self.find_channel(&msg.channel) {
            if let Err(e) = ch.send_message(reply).await {
                tracing::error!(
                    channel = ch.name(),
                    error = %e,
                    "failed to send reply / 发送回复失败"
                );
            }
        } else {
            tracing::warn!(channel = %msg.channel, "no matching channel for reply / 未找到匹配的回复频道");
        }

        // Decrement active sessions before releasing the lock
        self.metrics
            .active_sessions
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

        drop(_guard);
    }

    /// Streaming variant of process_message — returns a receiver of text deltas.
    ///
    /// Performs the same dedup, rate-limit, per-chat lock, history, and
    /// persistence steps as `process_message`, but calls
    /// `agent.handle_streaming()` and feeds deltas to the returned channel.
    pub async fn process_message_streaming(
        &self,
        msg: InboundMessage,
        buffer_size: usize,
    ) -> Result<mpsc::Receiver<String>, RequestValidationError> {
        let mut msg = msg;
        if msg.trace_id.is_empty() {
            msg.trace_id = uuid::Uuid::new_v4().to_string();
        }

        if !msg.message_id.is_empty() {
            let mut recent = self.recent_ids.lock().await;
            if recent.get(&msg.message_id).is_some() {
                self.forget_prevalidated_message(&msg.message_id);
                tracing::debug!(msg_id = %msg.message_id, "gateway: duplicate streaming request, skipping");
                let (_delta_tx, delta_rx) = mpsc::channel::<String>(buffer_size.max(1));
                return Ok(delta_rx);
            }
            recent.put(msg.message_id.clone(), ());
        }

        // Metrics
        self.metrics
            .total_requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let session_key = self.session_key(&msg);

        let prevalidated = self.take_prevalidated_message(&msg.message_id);
        if !prevalidated {
            self.validate_request(&msg, &session_key)?;
        }

        // Per-chat lock — acquired inside the spawned task to serialise processing
        let lock = self
            .chat_locks
            .entry(session_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        // Create delta channel
        let (delta_tx, delta_rx) = mpsc::channel::<String>(buffer_size);

        // Spawn agent streaming task — acquires per-chat lock, loads history, runs agent
        let agent = self.agent.clone();
        let memory = self.memory.clone();
        let history_limit = self.config.memory.history_limit as usize;
        let metrics = self.metrics.clone();
        let channels = self.channels.clone();
        let channel_name = msg.channel.clone();
        tokio::spawn(async move {
            let _guard = lock.lock().await;

            metrics
                .active_sessions
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // Acknowledge
            if let Some(ch) = channels.get(&channel_name)
                && let Err(e) = ch.acknowledge(&msg).await
            {
                tracing::debug!(error = %e, "acknowledge failed (non-critical) / 确认失败（非关键）");
            }

            // Load history
            let history = memory
                .get_history(&session_key, history_limit)
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(error = %e, "failed to load history / 加载历史记录失败");
                    vec![]
                });

            let (_, persist_messages) = agent.handle_streaming(&msg, &history, delta_tx).await;

            if !persist_messages.is_empty()
                && let Err(e) = memory
                    .save_conversation(&session_key, &persist_messages)
                    .await
            {
                tracing::error!(error = %e, "failed to save streaming conversation / 保存流式对话失败");
            }

            metrics
                .active_sessions
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });

        Ok(delta_rx)
    }
}

/// Errors that can occur when validating an inbound request through the Gateway.
#[derive(Debug)]
pub enum RequestValidationError {
    RateLimited,
    MessageTooLong { len: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evict_oldest_rate_limiter_entries_keeps_newest_sessions() {
        let rate_limiter = DashMap::new();
        let now = Instant::now();
        rate_limiter.insert(
            "session-old".to_string(),
            vec![now.checked_sub(std::time::Duration::from_secs(30)).unwrap()],
        );
        rate_limiter.insert(
            "session-mid".to_string(),
            vec![now.checked_sub(std::time::Duration::from_secs(20)).unwrap()],
        );
        rate_limiter.insert(
            "session-new".to_string(),
            vec![now.checked_sub(std::time::Duration::from_secs(10)).unwrap()],
        );

        let evicted = evict_oldest_rate_limiter_entries(&rate_limiter, 2);

        assert_eq!(evicted, 1);
        assert!(!rate_limiter.contains_key("session-old"));
        assert!(rate_limiter.contains_key("session-mid"));
        assert!(rate_limiter.contains_key("session-new"));
    }
}
