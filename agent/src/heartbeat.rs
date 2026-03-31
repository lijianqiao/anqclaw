//! Heartbeat adapter — converts legacy heartbeat config into a scheduler task.

use std::path::Path;
use std::time::Duration;

use crate::config::{HeartbeatSection, SchedulerTaskConfig};

pub const HEARTBEAT_TASK_NAME: &str = "heartbeat";

#[derive(Debug, Clone)]
pub struct HeartbeatTask {
    pub config: SchedulerTaskConfig,
    pub interval: Duration,
}

pub fn build_heartbeat_task(
    config: &HeartbeatSection,
    workspace_path: &str,
) -> Option<HeartbeatTask> {
    if !config.enabled {
        return None;
    }

    let interval_minutes = config.interval_minutes.max(1);
    Some(HeartbeatTask {
        config: SchedulerTaskConfig {
            name: HEARTBEAT_TASK_NAME.to_string(),
            cron: String::new(),
            prompt_file: Path::new(workspace_path)
                .join("HEARTBEAT.md")
                .to_string_lossy()
                .into_owned(),
            prompt: String::new(),
            notify_channel: config.notify_channel.clone(),
            notify_chat_id: config.notify_chat_id.clone(),
            enabled: true,
        },
        interval: Duration::from_secs(u64::from(interval_minutes) * 60),
    })
}
