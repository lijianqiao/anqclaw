//! `activate_skill` tool — loads a skill's full prompt on demand.
//!
//! When the LLM determines a user's request matches a skill, it calls this tool
//! to load the complete skill prompt. The returned content is injected into the
//! conversation as a tool result, giving the LLM the full context to respond.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::skill::SkillRegistry;

use super::Tool;

pub struct ActivateSkill {
    registry: Arc<SkillRegistry>,
}

impl ActivateSkill {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
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

            self.registry.load_content(skill_name)
        })
    }
}
