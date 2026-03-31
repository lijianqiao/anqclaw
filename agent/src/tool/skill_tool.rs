//! `activate_skill` tool — compatibility path for explicitly loading a skill.
//!
//! The preferred mainline is now: model scans `<available_skills>` and reads
//! `SKILL.md` via `file_read`. This tool remains available for compatibility
//! and debugging when an explicit skill activation is still needed.
//!
//! Tracks activated skills and enforces the `max_active_skills` limit.

use anyhow::Result;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::skill::SkillRegistry;

use super::Tool;

pub struct ActivateSkill {
    registry: Arc<SkillRegistry>,
    max_active: u32,
    /// Tracks the order of activated skills (FIFO). When exceeding max_active,
    /// the oldest skill is automatically released.
    active: Mutex<VecDeque<String>>,
}

impl ActivateSkill {
    pub fn new(registry: Arc<SkillRegistry>, max_active_skills: u32) -> Self {
        Self {
            registry,
            max_active: max_active_skills,
            active: Mutex::new(VecDeque::new()),
        }
    }
}

impl Tool for ActivateSkill {
    fn name(&self) -> &str {
        "activate_skill"
    }

    fn description(&self) -> &str {
        "Compatibility/debug tool: explicitly load a skill prompt by name. Prefer reading the SKILL.md path exposed in <available_skills>; use this only when explicit activation is required."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Name of the skill to activate (from the Available Skills list)"
                }
            },
            "required": ["skill_name"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let skill_name = args
                .get("skill_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("missing `skill_name` parameter / 缺少 `skill_name` 参数")
                })?;

            let content = self.registry.load_content(skill_name)?;

            // Track activation and enforce limit
            let mut active = self.active.lock().await;

            // If already active, move to back (most recent)
            if let Some(pos) = active.iter().position(|s| s == skill_name) {
                active.remove(pos);
            }
            active.push_back(skill_name.to_string());

            // Evict oldest if over limit
            let mut released = Vec::new();
            while active.len() as u32 > self.max_active && self.max_active > 0 {
                if let Some(old) = active.pop_front() {
                    released.push(old);
                }
            }

            if !released.is_empty() {
                tracing::info!(
                    released = ?released,
                    active = ?active.iter().collect::<Vec<_>>(),
                    "skills evicted to enforce max_active_skills limit / 已驱逐技能以满足最大活跃技能限制"
                );
            }

            Ok(content)
        })
    }
}
