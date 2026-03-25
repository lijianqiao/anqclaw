//! Scheduler — cron-based multi-task runner that supersedes the simple Heartbeat.
//!
//! Each task has a cron expression, a prompt (inline or from file), and a
//! notification channel + chat_id.  The Heartbeat concept is now just one
//! possible scheduled task.
//!
//! Flow per tick: cron fires → read prompt → build InboundMessage →
//!                Agent.handle() → persist conversation → notify channel
//!                (skip notification if reply contains "HEARTBEAT_OK").

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use cron::Schedule;

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::config::SchedulerTaskConfig;
use crate::memory::MemoryStore;
use crate::paths::resolve_path;
use crate::types::InboundMessage;

// ─── Scheduler ──────────────────────────────────────────────────────────────

pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    channels: Vec<Arc<dyn Channel>>,
}

struct ScheduledTask {
    config: SchedulerTaskConfig,
    schedule: Schedule,
}

impl Scheduler {
    /// Creates a new scheduler from config.  Invalid cron expressions are
    /// logged and skipped (the task is not registered).
    pub fn new(
        tasks: &[SchedulerTaskConfig],
        agent: Arc<AgentCore>,
        memory: Arc<MemoryStore>,
        channels: Vec<Arc<dyn Channel>>,
        home: &std::path::Path,
    ) -> Self {
        let mut scheduled = Vec::new();

        for cfg in tasks {
            if !cfg.enabled {
                tracing::info!(name = %cfg.name, "scheduler: task disabled, skipping");
                continue;
            }

            // Resolve prompt_file relative to home
            let mut resolved_cfg = cfg.clone();
            if !resolved_cfg.prompt_file.is_empty() {
                resolved_cfg.prompt_file = resolve_path(home, &resolved_cfg.prompt_file)
                    .to_string_lossy()
                    .into_owned();
            }

            match cfg.cron.parse::<Schedule>() {
                Ok(schedule) => {
                    tracing::info!(
                        name = %cfg.name,
                        cron = %cfg.cron,
                        "scheduler: task registered"
                    );
                    scheduled.push(ScheduledTask {
                        config: resolved_cfg,
                        schedule,
                    });
                }
                Err(e) => {
                    tracing::error!(
                        name = %cfg.name,
                        cron = %cfg.cron,
                        error = %e,
                        "scheduler: invalid cron expression, skipping task"
                    );
                }
            }
        }

        Self {
            tasks: scheduled,
            agent,
            memory,
            channels,
        }
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Runs the scheduler loop forever, checking every 30 seconds for tasks
    /// whose next fire time has passed.
    pub async fn run(&self) -> anyhow::Result<()> {
        if self.tasks.is_empty() {
            tracing::info!("scheduler: no tasks registered, exiting");
            return Ok(());
        }

        tracing::info!(
            count = self.tasks.len(),
            "scheduler: started"
        );

        // Track last-fired time for each task to prevent double-firing
        let mut last_fired: Vec<chrono::DateTime<Utc>> = vec![Utc::now(); self.tasks.len()];

        let mut interval = tokio::time::interval(Duration::from_secs(30));
        // Consume the immediate first tick
        interval.tick().await;

        loop {
            interval.tick().await;
            let now = Utc::now();

            for (i, task) in self.tasks.iter().enumerate() {
                // Find the next fire time after the last fired time
                if let Some(next) = task.schedule.after(&last_fired[i]).next() {
                    if next <= now {
                        last_fired[i] = now;
                        self.run_task(task).await;
                    }
                }
            }
        }
    }

    async fn run_task(&self, task: &ScheduledTask) {
        let prompt = match self.load_prompt(&task.config) {
            Some(p) if !p.trim().is_empty() => p,
            _ => {
                tracing::debug!(
                    name = %task.config.name,
                    "scheduler: prompt empty or missing, skipping"
                );
                return;
            }
        };

        tracing::info!(name = %task.config.name, "scheduler: running task");

        let chat_id = format!("__scheduler__{}", task.config.name);
        let msg = InboundMessage {
            channel: "__scheduler__".into(),
            chat_id: chat_id.clone(),
            sender_id: "__system__".into(),
            message_id: String::new(),
            content: crate::types::MessageContent::Text(prompt),
            timestamp: Utc::now().timestamp(),
        };

        // Load task-specific history
        let history = self
            .memory
            .get_history(&chat_id, 5)
            .await
            .unwrap_or_default();

        let (mut reply, conversation) = self.agent.handle(&msg, &history).await;

        // Persist conversation
        if !conversation.is_empty() {
            if let Err(e) = self.memory.save_conversation(&chat_id, &conversation).await {
                tracing::error!(
                    name = %task.config.name,
                    error = %e,
                    "scheduler: failed to save conversation"
                );
            }
        }

        // "HEARTBEAT_OK" convention — skip notification
        if reply.content.contains("HEARTBEAT_OK") {
            tracing::debug!(
                name = %task.config.name,
                "scheduler: HEARTBEAT_OK — no notification"
            );
            return;
        }

        // Route notification
        if task.config.notify_chat_id.is_empty() {
            tracing::debug!(
                name = %task.config.name,
                "scheduler: no notify_chat_id configured, skipping notification"
            );
            return;
        }

        reply.chat_id = task.config.notify_chat_id.clone();
        reply.channel = task.config.notify_channel.clone();

        if let Some(ch) = self.channels.iter().find(|c| c.name() == task.config.notify_channel) {
            if let Err(e) = ch.send_message(reply).await {
                tracing::error!(
                    name = %task.config.name,
                    channel = %task.config.notify_channel,
                    error = %e,
                    "scheduler: failed to send notification"
                );
            }
        } else {
            tracing::warn!(
                name = %task.config.name,
                channel = %task.config.notify_channel,
                "scheduler: no matching channel for notification"
            );
        }
    }

    /// Load prompt from `prompt_file` (priority) or inline `prompt`.
    fn load_prompt(&self, config: &SchedulerTaskConfig) -> Option<String> {
        // Priority 1: prompt_file
        if !config.prompt_file.is_empty() {
            match std::fs::read_to_string(&config.prompt_file) {
                Ok(content) => return Some(content),
                Err(e) => {
                    tracing::warn!(
                        name = %config.name,
                        path = %config.prompt_file,
                        error = %e,
                        "scheduler: prompt_file read failed, trying inline prompt"
                    );
                }
            }
        }

        // Priority 2: inline prompt
        if !config.prompt.is_empty() {
            return Some(config.prompt.clone());
        }

        None
    }
}
