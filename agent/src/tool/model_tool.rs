//! `switch_model` tool — dynamically switch the LLM profile within a session.

use anyhow::Result;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use super::Tool;

/// Tool that lets the LLM request a model switch mid-conversation.
///
/// Validation-only: checks that the profile name exists and returns it.
/// The actual client swap happens in the agent loop after tool execution.
pub struct SwitchModel {
    available_profiles: HashSet<String>,
}

impl SwitchModel {
    pub fn new(profile_names: Vec<String>) -> Self {
        Self {
            available_profiles: profile_names.into_iter().collect(),
        }
    }
}

impl Tool for SwitchModel {
    fn name(&self) -> &str {
        "switch_model"
    }

    fn description(&self) -> &str {
        "Switch the LLM model to a different configured profile for the rest of this conversation. \
         Available profiles are listed in the parameters."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let profiles: Vec<&str> = self.available_profiles.iter().map(|s| s.as_str()).collect();
        serde_json::json!({
            "type": "object",
            "properties": {
                "profile_name": {
                    "type": "string",
                    "description": format!(
                        "The name of the LLM profile to switch to. Available: {}",
                        profiles.join(", ")
                    )
                }
            },
            "required": ["profile_name"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let profile_name = args
                .get("profile_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing required parameter: profile_name / 缺少必需参数: profile_name"))?;

            if !self.available_profiles.contains(profile_name) {
                let available: Vec<&str> =
                    self.available_profiles.iter().map(|s| s.as_str()).collect();
                anyhow::bail!(
                    "unknown profile '{}' / 未知配置 '{}'. Available: {}",
                    profile_name,
                    profile_name,
                    available.join(", ")
                );
            }

            Ok(format!("__switch_model:{profile_name}"))
        })
    }
}
