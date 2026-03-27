//! `shell_exec` tool — runs a shell command and returns stdout + stderr.
//!
//! Safety:
//! - Three-level permission model: Readonly, Supervised, Full.
//! - Commands are checked against allow/block lists depending on level.
//! - Blocked directories are enforced in all modes.
//! - A configurable timeout kills the process if it runs too long.

use anyhow::{Result, bail};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use super::Tool;

/// Built-in readonly commands — safe to run in any mode.
const READONLY_COMMANDS: &[&str] = &[
    "ls", "dir", "cat", "head", "tail", "grep", "find", "date", "whoami", "pwd", "wc", "sort",
    "uniq", "echo", "file", "stat", "type", "where", "hostname", "uname", "df", "du", "env",
    "printenv", "which",
];

/// Commands that are ALWAYS blocked regardless of permission level.
const ALWAYS_BLOCKED: &[&str] = &[
    "mkfs",
    "dd",
    "format",
    "shutdown",
    "reboot",
    "init",
    "systemctl",
    "halt",
    "poweroff",
];

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionLevel {
    Readonly,
    Supervised,
    Full,
}

impl PermissionLevel {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "readonly" => Self::Readonly,
            "full" => Self::Full,
            _ => Self::Supervised, // default
        }
    }
}

pub struct ShellExec {
    permission_level: PermissionLevel,
    /// For Readonly: only READONLY_COMMANDS
    /// For Supervised: READONLY_COMMANDS + extra_allowed + legacy allowed_commands
    /// For Full: everything except blocked
    allowed: HashSet<String>,
    /// Commands blocked even in Full mode
    blocked: HashSet<String>,
    /// Directories blocked from any path arguments
    blocked_dirs: Vec<String>,
    /// Directories where Full permission applies regardless of base level
    trusted_dirs: Vec<String>,
    timeout: Duration,
}

impl ShellExec {
    pub fn new(
        permission_level: &str,
        legacy_allowed_commands: &[String],
        extra_allowed: &[String],
        blocked_commands: &[String],
        blocked_dirs: Vec<String>,
        trusted_dirs: Vec<String>,
        timeout_secs: u32,
    ) -> Self {
        let level = PermissionLevel::parse(permission_level);

        let mut allowed = HashSet::new();
        let mut blocked: HashSet<String> = ALWAYS_BLOCKED.iter().map(|s| s.to_string()).collect();
        blocked.extend(blocked_commands.iter().cloned());

        match level {
            PermissionLevel::Readonly => {
                for cmd in READONLY_COMMANDS {
                    allowed.insert(cmd.to_string());
                }
            }
            PermissionLevel::Supervised => {
                // Readonly base
                for cmd in READONLY_COMMANDS {
                    allowed.insert(cmd.to_string());
                }
                // Add legacy allowed_commands for backward compat
                for cmd in legacy_allowed_commands {
                    allowed.insert(cmd.clone());
                }
                // Add extra allowed
                for cmd in extra_allowed {
                    allowed.insert(cmd.clone());
                }
            }
            PermissionLevel::Full => {
                // In full mode, `allowed` is not checked — only `blocked` matters
            }
        }

        Self {
            permission_level: level,
            allowed,
            blocked,
            blocked_dirs,
            trusted_dirs,
            timeout: Duration::from_secs(timeout_secs as u64),
        }
    }

    /// Check if a command's path arguments reference blocked directories.
    fn check_blocked_dirs(&self, command: &str) -> Result<()> {
        for dir in &self.blocked_dirs {
            if command.contains(dir.as_str()) {
                bail!("command references blocked directory: {dir}");
            }
        }
        Ok(())
    }

    /// Check all sub-commands in a command chain against blocked/allowed lists.
    fn check_command_chain(&self, command: &str) -> Result<()> {
        let segments = split_command_chain(command);
        for segment in &segments {
            let first_token = segment.split_whitespace().next().unwrap_or("");

            // Check blocked commands (applies to ALL modes)
            if self.blocked.contains(first_token) {
                bail!("command `{first_token}` is blocked for safety reasons");
            }

            // Check if the segment starts with any blocked pattern
            for blocked in &self.blocked {
                if segment.starts_with(blocked.as_str()) {
                    bail!("command pattern `{blocked}` is blocked for safety reasons");
                }
            }

            // Permission check — per-segment, not whole command
            let in_trusted = !self.trusted_dirs.is_empty()
                && self
                    .trusted_dirs
                    .iter()
                    .any(|d| segment.contains(d.as_str()));

            match self.permission_level {
                PermissionLevel::Readonly | PermissionLevel::Supervised => {
                    if !in_trusted && !self.allowed.contains(first_token) {
                        bail!(
                            "command `{first_token}` is not allowed in {:?} mode. Allowed: {:?}",
                            self.permission_level,
                            self.allowed
                        );
                    }
                }
                PermissionLevel::Full => {}
            }
        }
        Ok(())
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `command` parameter"))?;

        // Check all sub-commands in the chain (pipes, &&, || etc.)
        self.check_command_chain(command)?;

        // Check blocked directories
        self.check_blocked_dirs(command)?;

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

/// Split a command string into sub-commands by shell operators.
///
/// Splits on `|`, `&&`, `||` (all platforms).
/// Unix additionally splits on `;`.
/// Windows additionally splits on single `&` (but not `&&`).
/// Quoted sections (single or double quotes) are preserved as-is.
pub fn split_command_chain(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(c) = chars.next() {
        if in_single_quote {
            current.push(c);
            if c == '\'' {
                in_single_quote = false;
            }
            continue;
        }
        if in_double_quote {
            if c == '\\' {
                current.push(c);
                if let Some(&next) = chars.peek() {
                    if next == '"' || next == '\\' {
                        current.push(chars.next().unwrap());
                    }
                }
            } else if c == '"' {
                current.push(c);
                in_double_quote = false;
            } else {
                current.push(c);
            }
            continue;
        }

        match c {
            '\'' => {
                current.push(c);
                in_single_quote = true;
            }
            '"' => {
                current.push(c);
                in_double_quote = true;
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next(); // consume second |
                }
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    segments.push(trimmed);
                }
                current.clear();
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next(); // consume second &
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        segments.push(trimmed);
                    }
                    current.clear();
                } else {
                    // Single & — split on Windows only
                    #[cfg(target_os = "windows")]
                    {
                        let trimmed = current.trim().to_string();
                        if !trimmed.is_empty() {
                            segments.push(trimmed);
                        }
                        current.clear();
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        current.push(c);
                    }
                }
            }
            ';' => {
                #[cfg(not(target_os = "windows"))]
                {
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        segments.push(trimmed);
                    }
                    current.clear();
                }
                #[cfg(target_os = "windows")]
                {
                    current.push(c);
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        segments.push(trimmed);
    }

    segments
}

impl Tool for ShellExec {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its stdout, stderr, and exit code. Commands are subject to permission level restrictions."
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shell(level: &str) -> ShellExec {
        ShellExec::new(
            level,
            &[],
            &[],
            &["rm".to_string()],
            vec!["/etc".to_string()],
            vec![],
            5,
        )
    }

    #[test]
    fn test_permission_level_parse() {
        assert_eq!(
            PermissionLevel::parse("readonly"),
            PermissionLevel::Readonly
        );
        assert_eq!(PermissionLevel::parse("full"), PermissionLevel::Full);
        assert_eq!(
            PermissionLevel::parse("supervised"),
            PermissionLevel::Supervised
        );
        assert_eq!(
            PermissionLevel::parse("unknown"),
            PermissionLevel::Supervised
        );
    }

    #[tokio::test]
    async fn test_readonly_allows_ls() {
        let shell = make_shell("readonly");
        let args = serde_json::json!({ "command": "echo hello" });
        let result = shell.do_execute(args).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_readonly_blocks_unknown_command() {
        let shell = make_shell("readonly");
        let args = serde_json::json!({ "command": "cargo build" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not allowed"));
    }

    #[tokio::test]
    async fn test_blocked_command() {
        let shell = make_shell("full");
        let args = serde_json::json!({ "command": "rm -rf /" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn test_always_blocked_commands() {
        let shell = make_shell("full");
        let args = serde_json::json!({ "command": "shutdown -h now" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_blocked_dir_in_command() {
        let shell = make_shell("full");
        let args = serde_json::json!({ "command": "cat /etc/passwd" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("blocked directory")
        );
    }

    #[tokio::test]
    async fn test_missing_command_param() {
        let shell = make_shell("full");
        let args = serde_json::json!({});
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
    }

    // ── split_command_chain tests ────────────────────────────────────────────

    #[test]
    fn test_split_simple_pipe() {
        let result = split_command_chain("ls | grep foo");
        assert_eq!(result, vec!["ls", "grep foo"]);
    }

    #[test]
    fn test_split_and_chain() {
        let result = split_command_chain("echo a && echo b");
        assert_eq!(result, vec!["echo a", "echo b"]);
    }

    #[test]
    fn test_split_or_chain() {
        let result = split_command_chain("cmd1 || cmd2");
        assert_eq!(result, vec!["cmd1", "cmd2"]);
    }

    #[test]
    fn test_split_semicolon() {
        let result = split_command_chain("echo a; echo b");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, vec!["echo a", "echo b"]);
        #[cfg(target_os = "windows")]
        assert_eq!(result, vec!["echo a; echo b"]);
    }

    #[test]
    fn test_split_ampersand() {
        let result = split_command_chain("echo a & echo b");
        #[cfg(target_os = "windows")]
        assert_eq!(result, vec!["echo a", "echo b"]);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, vec!["echo a & echo b"]);
    }

    #[test]
    fn test_split_preserves_quotes() {
        let result = split_command_chain("echo 'a | b' | cat");
        assert_eq!(result, vec!["echo 'a | b'", "cat"]);
    }

    #[tokio::test]
    async fn test_check_chain_blocks_dangerous() {
        let shell = make_shell("full");
        let args = serde_json::json!({ "command": "echo hello | rm -rf /" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn test_check_chain_allows_safe() {
        let shell = make_shell("full");
        let args = serde_json::json!({ "command": "echo hello | grep hello" });
        let result = shell.do_execute(args).await;
        assert!(result.is_ok());
    }
}
