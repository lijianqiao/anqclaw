//! `activate_skill` tool — loads a skill's full prompt on demand.
//!
//! When the LLM determines a user's request matches a skill, it calls this tool
//! to load the complete skill prompt. The returned content is injected into the
//! conversation as a tool result, giving the LLM the full context to respond.
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
        "Load a specialized skill prompt by name. Call this BEFORE responding when the user's request matches a skill. Returns the full skill prompt content."
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
                .ok_or_else(|| anyhow::anyhow!("missing `skill_name` parameter"))?;

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
                    "skills evicted to enforce max_active_skills limit"
                );
            }

            Ok(content)
        })
    }
}
