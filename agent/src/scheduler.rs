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

use chrono::{Duration as ChronoDuration, Utc};
use cron::Schedule;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentCore;
use crate::channel::Channel;
use crate::config::SchedulerTaskConfig;
use crate::heartbeat::{HEARTBEAT_TASK_NAME, HeartbeatTask};
use crate::memory::MemoryStore;
use crate::paths::resolve_path;
use crate::types::InboundMessage;

const SCHEDULER_HISTORY_LIMIT: usize = 5;
const SCHEDULER_MIN_SLEEP_MILLIS: u64 = 100;
const SCHEDULER_OVERDUE_MIN_BACKOFF_MILLIS: u64 = 200;
const SCHEDULER_OVERDUE_MAX_BACKOFF_SECS: u64 = 30;

// ─── Scheduler ──────────────────────────────────────────────────────────────

pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    channels: Vec<Arc<dyn Channel>>,
}

struct ScheduledTask {
    config: SchedulerTaskConfig,
    schedule: TaskSchedule,
}

enum TaskSchedule {
    Cron(Box<Schedule>),
    FixedInterval(Duration),
}

impl Scheduler {
    /// Creates a new scheduler from config.  Invalid cron expressions are
    /// logged and skipped (the task is not registered).
    pub fn new(
        tasks: &[SchedulerTaskConfig],
        heartbeat_task: Option<HeartbeatTask>,
        agent: Arc<AgentCore>,
        memory: Arc<MemoryStore>,
        channels: Vec<Arc<dyn Channel>>,
        home: &std::path::Path,
    ) -> Self {
        let mut scheduled = Vec::new();

        for cfg in tasks {
            if !cfg.enabled {
                tracing::info!(name = %cfg.name, "scheduler: task disabled, skipping / 调度器: 任务已禁用，已跳过");
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
                        "scheduler: task registered / 调度器: 任务已注册"
                    );
                    scheduled.push(ScheduledTask {
                        config: resolved_cfg,
                        schedule: TaskSchedule::Cron(Box::new(schedule)),
                    });
                }
                Err(e) => {
                    tracing::error!(
                        name = %cfg.name,
                        cron = %cfg.cron,
                        error = %e,
                        "scheduler: invalid cron expression, skipping task / 调度器: 无效的 cron 表达式，已跳过任务"
                    );
                }
            }
        }

        if let Some(heartbeat_task) = heartbeat_task {
            tracing::info!(
                interval_secs = heartbeat_task.interval.as_secs(),
                path = heartbeat_task.config.prompt_file.as_str(),
                "scheduler: heartbeat task registered / 调度器: 心跳任务已注册"
            );
            scheduled.push(ScheduledTask {
                config: heartbeat_task.config,
                schedule: TaskSchedule::FixedInterval(heartbeat_task.interval),
            });
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

    /// Runs the scheduler loop until the shutdown token is cancelled,
    /// sleeping until the next scheduled task instead of fixed-interval polling.
    pub async fn run(&self, shutdown: CancellationToken) -> anyhow::Result<()> {
        if self.tasks.is_empty() {
            tracing::info!("scheduler: no tasks registered, exiting / 调度器: 无注册任务，退出");
            return Ok(());
        }

        tracing::info!(
            count = self.tasks.len(),
            "scheduler: started / 调度器: 已启动"
        );

        let now = Utc::now();
        let mut next_runs: Vec<chrono::DateTime<Utc>> = self
            .tasks
            .iter()
            .map(|task| task.next_run_after(now))
            .collect();
        let mut consecutive_overdue_runs = vec![0u32; self.tasks.len()];

        loop {
            let now = Utc::now();
            for (i, task) in self.tasks.iter().enumerate() {
                if next_runs[i] <= now {
                    next_runs[i] = task.next_run_after(now);
                    self.run_task(task).await;
                    if next_runs[i] <= Utc::now() {
                        consecutive_overdue_runs[i] = consecutive_overdue_runs[i].saturating_add(1);
                    } else {
                        consecutive_overdue_runs[i] = 0;
                    }
                } else {
                    consecutive_overdue_runs[i] = 0;
                }
            }

            let sleep_reference = Utc::now();
            let sleep_duration = next_runs
                .iter()
                .enumerate()
                .map(|(index, next_run)| {
                    scheduler_sleep_duration(
                        *next_run,
                        sleep_reference,
                        consecutive_overdue_runs[index],
                    )
                })
                .min()
                .unwrap_or_else(|| Duration::from_millis(SCHEDULER_MIN_SLEEP_MILLIS));

            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    tracing::info!("scheduler: shutdown signal received / 调度器: 收到关闭信号");
                    break;
                }
                _ = tokio::time::sleep(sleep_duration) => {}
            }
        }

        Ok(())
    }

    async fn run_task(&self, task: &ScheduledTask) {
        let prompt = match self.load_prompt(&task.config) {
            Some(p) if !p.trim().is_empty() => p,
            _ => {
                tracing::debug!(
                    name = %task.config.name,
                    "scheduler: prompt empty or missing, skipping / 调度器: 提示词为空或缺失，已跳过"
                );
                return;
            }
        };

        tracing::info!(name = %task.config.name, "scheduler: running task / 调度器: 正在执行任务");

        let (chat_id, msg) = if task.is_heartbeat_task() {
            let msg = InboundMessage::heartbeat(&prompt);
            (msg.chat_id.clone(), msg)
        } else {
            let chat_id = format!("__scheduler__{}", task.config.name);
            let msg = InboundMessage {
                channel: "__scheduler__".into(),
                chat_id: chat_id.clone(),
                sender_id: "__system__".into(),
                message_id: String::new(),
                content: crate::types::MessageContent::Text(prompt),
                timestamp: Utc::now().timestamp(),
                trace_id: String::new(),
                images: vec![],
            };
            (chat_id, msg)
        };

        // Load task-specific history
        let history = self
            .memory
            .get_history(&chat_id, SCHEDULER_HISTORY_LIMIT)
            .await
            .unwrap_or_default();

        let (mut reply, conversation) = self.agent.handle(&msg, &history).await;

        // Persist conversation
        if !conversation.is_empty()
            && let Err(e) = self.memory.save_conversation(&chat_id, &conversation).await
        {
            tracing::error!(
                name = %task.config.name,
                error = %e,
                "scheduler: failed to save conversation / 调度器: 保存对话失败"
            );
        }

        // "HEARTBEAT_OK" convention — skip notification
        if reply.content.contains("HEARTBEAT_OK") {
            tracing::debug!(
                name = %task.config.name,
                "scheduler: HEARTBEAT_OK, no notification / 调度器: HEARTBEAT_OK，无需通知"
            );
            return;
        }

        // Route notification
        if task.config.notify_chat_id.is_empty() {
            tracing::debug!(
                name = %task.config.name,
                "scheduler: no notify_chat_id configured, skipping notification / 调度器: 未配置 notify_chat_id，已跳过通知"
            );
            return;
        }

        reply.chat_id = task.config.notify_chat_id.clone();
        reply.channel = task.config.notify_channel.clone();

        if let Some(ch) = self
            .channels
            .iter()
            .find(|c| c.name() == task.config.notify_channel)
        {
            if let Err(e) = ch.send_message(reply).await {
                tracing::error!(
                    name = %task.config.name,
                    channel = %task.config.notify_channel,
                    error = %e,
                    "scheduler: failed to send notification / 调度器: 发送通知失败"
                );
            }
        } else {
            tracing::warn!(
                name = %task.config.name,
                channel = %task.config.notify_channel,
                "scheduler: no matching channel for notification / 调度器: 未找到匹配的通知频道"
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
                        "scheduler: prompt_file read failed, trying inline prompt / 调度器: 提示词文件读取失败，尝试内联提示词"
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

impl ScheduledTask {
    fn is_heartbeat_task(&self) -> bool {
        self.config.name == HEARTBEAT_TASK_NAME
    }

    fn next_run_after(&self, after: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
        match &self.schedule {
            TaskSchedule::Cron(schedule) => schedule.after(&after).next().unwrap_or(after),
            TaskSchedule::FixedInterval(interval) => after + duration_to_chrono(*interval),
        }
    }
}

fn duration_to_chrono(duration: Duration) -> ChronoDuration {
    ChronoDuration::from_std(duration).unwrap_or_else(|_| ChronoDuration::seconds(i64::MAX))
}

fn scheduler_sleep_duration(
    next_run: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
    consecutive_overdue_runs: u32,
) -> Duration {
    match next_run.signed_duration_since(now).to_std() {
        Ok(delay) => delay.max(Duration::from_millis(SCHEDULER_MIN_SLEEP_MILLIS)),
        Err(_) => scheduler_overdue_backoff(consecutive_overdue_runs),
    }
}

fn scheduler_overdue_backoff(consecutive_overdue_runs: u32) -> Duration {
    let exponent = consecutive_overdue_runs.saturating_sub(1).min(8);
    let millis = SCHEDULER_OVERDUE_MIN_BACKOFF_MILLIS.saturating_mul(1u64 << exponent);
    Duration::from_millis(millis).min(Duration::from_secs(SCHEDULER_OVERDUE_MAX_BACKOFF_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_sleep_duration_uses_positive_delta() {
        let now = Utc::now();
        let next_run = now + ChronoDuration::seconds(2);

        let sleep = scheduler_sleep_duration(next_run, now, 0);

        assert!(sleep >= Duration::from_secs(2));
    }

    #[test]
    fn test_scheduler_sleep_duration_applies_overdue_backoff() {
        let now = Utc::now();
        let next_run = now - ChronoDuration::seconds(5);

        let first_backoff = scheduler_sleep_duration(next_run, now, 1);
        let third_backoff = scheduler_sleep_duration(next_run, now, 3);

        assert_eq!(
            first_backoff,
            Duration::from_millis(SCHEDULER_OVERDUE_MIN_BACKOFF_MILLIS)
        );
        assert_eq!(third_backoff, Duration::from_millis(800));
    }

    #[test]
    fn test_scheduler_sleep_duration_caps_overdue_backoff() {
        let now = Utc::now();
        let next_run = now - ChronoDuration::seconds(5);

        let sleep = scheduler_sleep_duration(next_run, now, 99);

        assert_eq!(
            sleep,
            Duration::from_secs(SCHEDULER_OVERDUE_MAX_BACKOFF_SECS)
        );
    }
}
