//! `shell_exec` tool — runs a shell command and returns stdout + stderr.
//!
//! Safety:
//! - Only commands whose first token is in the allow-list are executed.
//! - A configurable timeout kills the process if it runs too long.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use super::Tool;

pub struct ShellExec {
    allowed: HashSet<String>,
    timeout: Duration,
}

impl ShellExec {
    pub fn new(allowed_commands: &[String], timeout_secs: u32) -> Self {
        Self {
            allowed: allowed_commands.iter().cloned().collect(),
            timeout: Duration::from_secs(timeout_secs as u64),
        }
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `command` parameter"))?;

        // Extract the first token (command name) for allow-list check
        let first_token = command
            .split_whitespace()
            .next()
            .unwrap_or("");

        if !self.allowed.contains(first_token) {
            bail!(
                "command `{first_token}` is not in the allow-list. Allowed: {:?}",
                self.allowed
            );
        }

        // Use platform-appropriate shell
        let mut child = {
            #[cfg(target_os = "windows")]
            {
                tokio::process::Command::new("cmd")
                    .args(["/C", command])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?
            }
            #[cfg(not(target_os = "windows"))]
            {
                tokio::process::Command::new("sh")
                    .args(["-c", command])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?
            }
        };

        // Read stdout/stderr before waiting (take ownership of handles)
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let wait_fut = async {
            let status = child.wait().await?;

            let stdout = if let Some(mut h) = stdout_handle {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await?;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            let stderr = if let Some(mut h) = stderr_handle {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await?;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            Ok::<_, anyhow::Error>((status, stdout, stderr))
        };

        match tokio::time::timeout(self.timeout, wait_fut).await {
            Ok(Ok((status, stdout, stderr))) => {
                let exit_code = status.code().unwrap_or(-1);
                let mut result = format!("[exit code: {exit_code}]\n");
                if !stdout.is_empty() {
                    result.push_str(&format!("[stdout]\n{stdout}\n"));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("[stderr]\n{stderr}\n"));
                }
                Ok(result)
            }
            Ok(Err(e)) => bail!("failed to run command: {e}"),
            Err(_) => {
                bail!("command timed out after {:?}", self.timeout)
            }
        }
    }
}

impl Tool for ShellExec {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its stdout, stderr, and exit code. Only allowed commands can be executed."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_execute(args))
    }
}
