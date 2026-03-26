//! Custom external tool — executes a shell command defined in config.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::config::CustomToolConfig;

use super::Tool;

pub struct CustomTool {
    name: String,
    description: String,
    command: String,
    timeout: Duration,
}

impl CustomTool {
    pub fn new(config: &CustomToolConfig) -> Self {
        Self {
            name: config.name.clone(),
            description: config.description.clone(),
            command: config.command.clone(),
            timeout: Duration::from_secs(config.timeout_secs as u64),
        }
    }
}

impl Tool for CustomTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "args": {
                    "type": "string",
                    "description": "Optional arguments to append to the command"
                }
            },
            "required": []
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let extra_args = args
                .get("args")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Build the full command — the config `command` is the base,
            // extra_args are appended as-is.
            let full_command = if extra_args.is_empty() {
                self.command.clone()
            } else {
                format!("{} {}", self.command, extra_args)
            };

            tracing::debug!(tool = %self.name, command = %full_command, "executing custom tool");

            let child = if cfg!(target_os = "windows") {
                tokio::process::Command::new("cmd")
                    .args(["/C", &full_command])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
            } else {
                tokio::process::Command::new("sh")
                    .args(["-c", &full_command])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
            };

            let child = child.map_err(|e| anyhow::anyhow!("failed to spawn command: {e}"))?;

            let output = tokio::time::timeout(self.timeout, child.wait_with_output())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "custom tool `{}` timed out after {}s",
                        self.name,
                        self.timeout.as_secs()
                    )
                })?
                .map_err(|e| anyhow::anyhow!("command execution failed: {e}"))?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                let mut result = stdout.into_owned();
                if !stderr.is_empty() {
                    result.push_str("\n[stderr] ");
                    result.push_str(&stderr);
                }
                Ok(result)
            } else {
                let code = output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                anyhow::bail!(
                    "command exited with code {code}\nstdout: {stdout}\nstderr: {stderr}"
                )
            }
        })
    }
}
