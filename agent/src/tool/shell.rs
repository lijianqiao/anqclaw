//! `shell_exec` tool — runs a shell command and returns stdout + stderr.
//!
//! Safety:
//! - Three-level permission model: Readonly, Supervised, Full.
//! - Commands are checked against allow/block lists depending on level.
//! - Blocked directories are enforced in all modes.
//! - A configurable timeout kills the process if it runs too long.

use anyhow::{Result, bail};
#[cfg(target_os = "windows")]
use encoding_rs::GBK;
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
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

const MANAGED_RUNTIME_COMMANDS: &[&str] = &["python", "python3", "pip", "pip3", "uv"];
const MANAGED_RUNTIME_TIMEOUT_SECS: u64 = 300;
const LOG_PREVIEW_CHARS: usize = 240;
#[cfg(target_os = "windows")]
const WINDOWS_SHELL_BUILTINS: &[&str] = &["echo", "dir", "type", "cd", "set"];

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
    trusted_dirs: Vec<PathBuf>,
    timeout: Duration,
    working_dir: Option<PathBuf>,
    /// Venv isolation: when set, pip/uv install commands are rewritten to run
    /// inside this venv, and python commands use the venv interpreter.
    venv_path: Option<String>,
    managed_python_version: Option<String>,
}

impl ShellExec {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        permission_level: &str,
        legacy_allowed_commands: &[String],
        extra_allowed: &[String],
        blocked_commands: &[String],
        blocked_dirs: Vec<String>,
        trusted_dirs: Vec<String>,
        timeout_secs: u32,
        working_dir: Option<String>,
        venv_path: Option<String>,
        managed_python_version: Option<String>,
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
                if venv_path.is_some() {
                    for cmd in MANAGED_RUNTIME_COMMANDS {
                        allowed.insert((*cmd).to_string());
                    }
                }
            }
            PermissionLevel::Full => {
                // In full mode, `allowed` is not checked — only `blocked` matters
            }
        }

        let working_dir = working_dir.map(|dir| {
            let path = Path::new(&dir);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                crate::paths::anqclaw_home().join(dir)
            }
        });

        Self {
            permission_level: level,
            allowed,
            blocked,
            blocked_dirs,
            trusted_dirs: trusted_dirs
                .into_iter()
                .map(|dir| crate::paths::resolve_configured_path(&dir))
                .filter_map(|dir| crate::paths::canonicalize_for_comparison(&dir).ok())
                .collect(),
            timeout: Duration::from_secs(timeout_secs as u64),
            working_dir,
            venv_path,
            managed_python_version,
        }
    }

    fn normalize_first_token(segment: &str) -> &str {
        segment
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('"')
            .trim_matches('\'')
    }

    fn preview_for_log(text: &str) -> String {
        let sanitized = text.replace('\r', " ").replace('\n', "\\n");
        let mut chars = sanitized.chars();
        let preview: String = chars.by_ref().take(LOG_PREVIEW_CHARS).collect();
        if chars.next().is_some() {
            format!("{preview}...[truncated]")
        } else {
            preview
        }
    }

    fn is_managed_runtime_command(&self, first_token: &str) -> bool {
        self.venv_path.is_some() && MANAGED_RUNTIME_COMMANDS.contains(&first_token)
    }

    fn uses_managed_python_runtime(command: &str) -> bool {
        let trimmed = command.trim();
        [
            "python",
            "python3",
            "pip",
            "pip3",
            "uv pip install",
            "uv run",
        ]
        .iter()
        .any(|prefix| trimmed == *prefix || trimmed.starts_with(&format!("{prefix} ")))
    }

    fn tokenize_command_segment(command: &str) -> Result<Vec<String>> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut chars = command.chars().peekable();
        let mut in_single_quote = false;
        let mut in_double_quote = false;

        while let Some(c) = chars.next() {
            if in_single_quote {
                if c == '\'' {
                    in_single_quote = false;
                } else {
                    current.push(c);
                }
                continue;
            }

            if in_double_quote {
                if c == '\\' {
                    if let Some(&next) = chars.peek() {
                        if next == '"' || next == '\\' {
                            current.push(chars.next().unwrap());
                        } else {
                            current.push(c);
                        }
                    } else {
                        current.push(c);
                    }
                } else if c == '"' {
                    in_double_quote = false;
                } else {
                    current.push(c);
                }
                continue;
            }

            match c {
                '\'' => in_single_quote = true,
                '"' => in_double_quote = true,
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            }
        }

        if in_single_quote || in_double_quote {
            bail!("unclosed quote in command")
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        if tokens.is_empty() {
            bail!("empty command")
        }
        Ok(tokens)
    }

    fn token_candidate_path(&self, token: &str) -> Option<PathBuf> {
        if token.is_empty() || token.starts_with('-') || token.contains('=') {
            return None;
        }

        let looks_like_path = token.starts_with('.')
            || token.starts_with('/')
            || token.starts_with("\\\\")
            || token.contains('/')
            || token.contains('\\')
            || Path::new(token).is_absolute();
        if !looks_like_path {
            return None;
        }

        let path = Path::new(token);
        Some(if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(dir) = &self.working_dir {
            dir.join(path)
        } else {
            crate::paths::anqclaw_home().join(path)
        })
    }

    fn segment_uses_trusted_dir(&self, segment: &str) -> bool {
        let Ok(tokens) = Self::tokenize_command_segment(segment) else {
            return false;
        };

        tokens
            .iter()
            .filter_map(|token| self.token_candidate_path(token))
            .any(|candidate| crate::paths::path_is_trusted(&candidate, &self.trusted_dirs))
    }

    fn decode_command_output(bytes: &[u8]) -> String {
        match String::from_utf8(bytes.to_vec()) {
            Ok(text) => text,
            Err(_) => {
                #[cfg(target_os = "windows")]
                {
                    let (decoded, _, _) = GBK.decode(bytes);
                    decoded.into_owned()
                }
                #[cfg(not(target_os = "windows"))]
                {
                    String::from_utf8_lossy(bytes).to_string()
                }
            }
        }
    }

    fn apply_process_environment(&self, cmd: &mut std::process::Command, managed_runtime: bool) {
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }

        #[cfg(target_os = "windows")]
        {
            if managed_runtime {
                cmd.env("PYTHONIOENCODING", "utf-8");
                cmd.env("PYTHONUTF8", "1");
            }
        }
    }

    fn apply_async_process_environment(
        &self,
        cmd: &mut tokio::process::Command,
        managed_runtime: bool,
    ) {
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }

        #[cfg(target_os = "windows")]
        {
            if managed_runtime {
                cmd.env("PYTHONIOENCODING", "utf-8");
                cmd.env("PYTHONUTF8", "1");
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn requires_shell(command: &str) -> bool {
        let first_token = Self::normalize_first_token(command.trim());
        if WINDOWS_SHELL_BUILTINS.contains(&first_token) {
            return true;
        }

        let mut chars = command.chars().peekable();
        let mut in_single_quote = false;
        let mut in_double_quote = false;

        while let Some(c) = chars.next() {
            if in_single_quote {
                if c == '\'' {
                    in_single_quote = false;
                }
                continue;
            }
            if in_double_quote {
                if c == '\\' {
                    if let Some(&next) = chars.peek()
                        && (next == '"' || next == '\\')
                    {
                        chars.next();
                    }
                    continue;
                }
                if c == '"' {
                    in_double_quote = false;
                }
                continue;
            }

            match c {
                '\'' => in_single_quote = true,
                '"' => in_double_quote = true,
                '|' | '&' | '<' | '>' | '%' => return true,
                _ => {}
            }
        }

        false
    }

    #[cfg(target_os = "windows")]
    fn tokenize_simple_command(command: &str) -> Result<Vec<String>> {
        Self::tokenize_command_segment(command)
    }

    #[cfg(target_os = "windows")]
    fn wrap_for_cmd(command: &str) -> String {
        let trimmed = command.trim();
        if trimmed.starts_with('"') {
            format!("\"{trimmed}\"")
        } else {
            trimmed.to_string()
        }
    }

    fn effective_timeout(&self, command: &str) -> Duration {
        if Self::uses_managed_python_runtime(command) {
            self.timeout
                .max(Duration::from_secs(MANAGED_RUNTIME_TIMEOUT_SECS))
        } else {
            self.timeout
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
            let first_token = Self::normalize_first_token(segment);

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
            let in_trusted = !self.trusted_dirs.is_empty() && self.segment_uses_trusted_dir(segment);

            match self.permission_level {
                PermissionLevel::Readonly | PermissionLevel::Supervised => {
                    if !in_trusted
                        && !self.allowed.contains(first_token)
                        && !self.is_managed_runtime_command(first_token)
                    {
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

    fn preferred_python_version(&self) -> &str {
        self.managed_python_version.as_deref().unwrap_or("3.12")
    }

    fn managed_venv_path(&self) -> Option<PathBuf> {
        let venv = self.venv_path.as_ref()?;
        let venv_path = Path::new(venv);
        Some(if venv_path.is_relative() {
            crate::paths::anqclaw_home().join(venv)
        } else {
            venv_path.to_path_buf()
        })
    }

    fn uv_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(home) = dirs::home_dir() {
            #[cfg(target_os = "windows")]
            candidates.push(home.join(".local").join("bin").join("uv.exe"));
            #[cfg(not(target_os = "windows"))]
            candidates.push(home.join(".local").join("bin").join("uv"));
        }
        candidates
    }

    fn existing_uv_path() -> Option<PathBuf> {
        let status = std::process::Command::new("uv")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if matches!(status, Ok(s) if s.success()) {
            tracing::info!(
                source = "PATH",
                path = "uv",
                "managed runtime: discovered uv"
            );
            return Some(PathBuf::from("uv"));
        }

        let candidate = Self::uv_candidates()
            .into_iter()
            .find(|candidate| candidate.exists());
        if let Some(ref path) = candidate {
            tracing::info!(path = %path.display(), source = "candidate", "managed runtime: discovered uv");
        }
        candidate
    }

    fn install_uv() -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            tracing::info!("managed runtime: installing uv via PowerShell installer");
            let output = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "$ProgressPreference='SilentlyContinue'; irm https://astral.sh/uv/install.ps1 | iex",
                ])
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::info!(
                        stderr_preview = %Self::preview_for_log(&stderr),
                        "managed runtime: uv installer completed"
                    );
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    tracing::warn!(
                        exit_code = ?output.status.code(),
                        stdout_preview = %Self::preview_for_log(&stdout),
                        stderr_preview = %Self::preview_for_log(&stderr),
                        "managed runtime: uv installer failed"
                    );
                    bail!("failed to install uv automatically via PowerShell installer")
                }
                Err(error) => {
                    tracing::warn!(error = %error, "managed runtime: failed to launch uv installer");
                    bail!("failed to install uv automatically via PowerShell installer")
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            tracing::info!("managed runtime: installing uv via shell installer");
            let output = std::process::Command::new("sh")
                .args([
                    "-c",
                    "if command -v curl >/dev/null 2>&1; then curl -LsSf https://astral.sh/uv/install.sh | sh; elif command -v wget >/dev/null 2>&1; then wget -qO- https://astral.sh/uv/install.sh | sh; else exit 127; fi",
                ])
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::info!(
                        stderr_preview = %Self::preview_for_log(&stderr),
                        "managed runtime: uv installer completed"
                    );
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    tracing::warn!(
                        exit_code = ?output.status.code(),
                        stdout_preview = %Self::preview_for_log(&stdout),
                        stderr_preview = %Self::preview_for_log(&stderr),
                        "managed runtime: uv installer failed"
                    );
                    bail!("failed to install uv automatically; curl or wget is required")
                }
                Err(error) => {
                    tracing::warn!(error = %error, "managed runtime: failed to launch uv installer");
                    bail!("failed to install uv automatically; curl or wget is required")
                }
            }
        }

        Self::existing_uv_path().ok_or_else(|| {
            anyhow::anyhow!("uv installer completed but uv binary is still unavailable")
        })
    }

    fn ensure_uv_available(&self) -> Result<PathBuf> {
        if let Some(path) = Self::existing_uv_path() {
            tracing::info!(path = %path.display(), "managed runtime: using existing uv");
            return Ok(path);
        }

        tracing::info!("managed runtime: uv not found, attempting bootstrap");
        Self::install_uv()
    }

    fn managed_python_binary(venv_abs: &Path) -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            venv_abs.join("Scripts").join("python.exe")
        }
        #[cfg(not(target_os = "windows"))]
        {
            venv_abs.join("bin").join("python")
        }
    }

    fn ensure_managed_python_runtime(&self) -> Result<PathBuf> {
        let Some(venv_abs) = self.managed_venv_path() else {
            bail!("managed Python runtime requested without a configured venv path")
        };

        let python_bin = Self::managed_python_binary(&venv_abs);
        if python_bin.exists() {
            tracing::info!(venv = %venv_abs.display(), python = %python_bin.display(), "managed runtime: existing venv is ready");
            return Ok(venv_abs);
        }

        if let Some(parent) = venv_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let uv_path = self.ensure_uv_available()?;
        let version = self.preferred_python_version().to_string();

        tracing::info!(venv = %venv_abs.display(), python = %version, uv = %uv_path.display(), "managed runtime: installing Python with uv");
        let mut install_command = std::process::Command::new(&uv_path);
        install_command.args(["python", "install", &version]);
        self.apply_process_environment(&mut install_command, true);
        let install_output = install_command.output();
        match install_output {
            Ok(output) if output.status.success() => {
                let stderr = Self::decode_command_output(&output.stderr);
                tracing::info!(
                    python = %version,
                    stderr_preview = %Self::preview_for_log(&stderr),
                    "managed runtime: Python installation step completed"
                );
            }
            Ok(output) => {
                let stderr = Self::decode_command_output(&output.stderr);
                let stdout = Self::decode_command_output(&output.stdout);
                tracing::warn!(
                    python = %version,
                    exit_code = ?output.status.code(),
                    stdout_preview = %Self::preview_for_log(&stdout),
                    stderr_preview = %Self::preview_for_log(&stderr),
                    "managed runtime: Python installation step failed"
                );
                bail!("failed to install managed Python {} with uv", version)
            }
            Err(error) => {
                tracing::warn!(error = %error, python = %version, "managed runtime: failed to launch `uv python install`");
                bail!("failed to install managed Python {} with uv", version)
            }
        }

        tracing::info!(venv = %venv_abs.display(), python = %version, "managed runtime: creating venv");
        let mut venv_command = std::process::Command::new(&uv_path);
        venv_command.args(["venv", "--python", &version, &venv_abs.to_string_lossy()]);
        self.apply_process_environment(&mut venv_command, true);
        let venv_output = venv_command.output();
        match venv_output {
            Ok(output) if output.status.success() => {
                let stderr = Self::decode_command_output(&output.stderr);
                tracing::info!(
                    venv = %venv_abs.display(),
                    stderr_preview = %Self::preview_for_log(&stderr),
                    "managed runtime: venv creation step completed"
                );
            }
            Ok(output) => {
                let stderr = Self::decode_command_output(&output.stderr);
                let stdout = Self::decode_command_output(&output.stdout);
                tracing::warn!(
                    venv = %venv_abs.display(),
                    exit_code = ?output.status.code(),
                    stdout_preview = %Self::preview_for_log(&stdout),
                    stderr_preview = %Self::preview_for_log(&stderr),
                    "managed runtime: venv creation step failed"
                );
                bail!("failed to create managed venv at {}", venv_abs.display())
            }
            Err(error) => {
                tracing::warn!(error = %error, venv = %venv_abs.display(), "managed runtime: failed to launch `uv venv`");
                bail!("failed to create managed venv at {}", venv_abs.display())
            }
        }

        if !python_bin.exists() {
            bail!(
                "managed Python bootstrap finished but interpreter is missing at {}",
                python_bin.display()
            )
        }

        tracing::info!(venv = %venv_abs.display(), python = %version, "managed Python runtime bootstrapped");
        Ok(venv_abs)
    }

    /// Rewrite a command to run inside the venv if it's a pip/uv install or
    /// python invocation. Replaces the bare `python`/`pip` with the venv's
    /// absolute path — no activate needed, works on all platforms.
    fn rewrite_for_venv(&self, command: &str) -> Result<(String, bool)> {
        let Some(_) = &self.venv_path else {
            return Ok((command.to_string(), false));
        };

        let trimmed = command.trim();

        // Detect which prefix to replace
        let (prefix, replacement_bin) = if trimmed.starts_with("pip3 ") {
            ("pip3", "pip")
        } else if trimmed.starts_with("pip ") {
            ("pip", "pip")
        } else if trimmed.starts_with("python3 ") {
            ("python3", "python")
        } else if trimmed.starts_with("python ") {
            ("python", "python")
        } else if trimmed.starts_with("uv pip install") {
            // uv pip install is fine as-is, just set --python to venv
            ("", "")
        } else {
            return Ok((command.to_string(), false));
        };

        let venv_abs = self.ensure_managed_python_runtime()?;
        let needs_create = !venv_abs.join("pyvenv.cfg").exists();

        // Build absolute path to the binary inside venv
        let bin_dir = if cfg!(target_os = "windows") {
            venv_abs.join("Scripts")
        } else {
            venv_abs.join("bin")
        };

        let rewritten = if trimmed.starts_with("uv pip install") {
            // uv pip install --python <venv_python> <rest>
            let rest = trimmed.strip_prefix("uv pip install").unwrap_or("");
            let venv_python = bin_dir.join(if cfg!(target_os = "windows") {
                "python.exe"
            } else {
                "python"
            });
            format!(
                "uv pip install --python \"{}\"{}",
                venv_python.display(),
                rest
            )
        } else {
            // Replace bare python/pip with venv absolute path
            let bin_path = bin_dir.join(if cfg!(target_os = "windows") {
                format!("{}.exe", replacement_bin)
            } else {
                replacement_bin.to_string()
            });
            let rest = trimmed.strip_prefix(prefix).unwrap_or(trimmed);
            format!("\"{}\"{}", bin_path.display(), rest)
        };

        tracing::info!(
            original = command,
            rewritten = %rewritten,
            venv = %venv_abs.display(),
            "shell: rewrote command for venv isolation"
        );

        Ok((rewritten, needs_create))
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

        // Rewrite pip/python commands to use venv if configured
        let (command, _venv_created) = self.rewrite_for_venv(command)?;
        let command = command.as_str();
        let timeout = self.effective_timeout(command);
        let managed_runtime = Self::uses_managed_python_runtime(command);

        tracing::info!(
            command = %command,
            managed_runtime,
            cwd = ?self.working_dir.as_ref().map(|p| p.display().to_string()),
            timeout_secs = timeout.as_secs(),
            "shell: starting command"
        );

        // Use platform-appropriate shell
        let mut child = {
            #[cfg(target_os = "windows")]
            {
                if !Self::requires_shell(command) {
                    let tokens = Self::tokenize_simple_command(command)?;
                    let mut cmd = tokio::process::Command::new(&tokens[0]);
                    cmd.args(&tokens[1..]);
                    self.apply_async_process_environment(&mut cmd, managed_runtime);
                    cmd.stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()?
                } else {
                    let mut cmd = tokio::process::Command::new("cmd");
                    cmd.args(["/S", "/C", &Self::wrap_for_cmd(command)]);
                    self.apply_async_process_environment(&mut cmd, managed_runtime);
                    cmd.stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()?
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let mut cmd = tokio::process::Command::new("sh");
                cmd.args(["-c", command]);
                self.apply_async_process_environment(&mut cmd, managed_runtime);
                cmd.stdout(std::process::Stdio::piped())
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
                Self::decode_command_output(&buf)
            } else {
                String::new()
            };

            let stderr = if let Some(mut h) = stderr_handle {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await?;
                Self::decode_command_output(&buf)
            } else {
                String::new()
            };

            Ok::<_, anyhow::Error>((status, stdout, stderr))
        };

        match tokio::time::timeout(timeout, wait_fut).await {
            Ok(Ok((status, stdout, stderr))) => {
                let exit_code = status.code().unwrap_or(-1);
                if exit_code == 0 {
                    tracing::info!(
                        command = %command,
                        managed_runtime,
                        exit_code,
                        stdout_bytes = stdout.len(),
                        stderr_bytes = stderr.len(),
                        "shell: command finished"
                    );
                } else {
                    tracing::warn!(
                        command = %command,
                        managed_runtime,
                        exit_code,
                        stdout_bytes = stdout.len(),
                        stderr_bytes = stderr.len(),
                        stderr_preview = %Self::preview_for_log(&stderr),
                        "shell: command finished with non-zero exit"
                    );
                }
                let mut result = format!("[exit code: {exit_code}]\n");
                if !stdout.is_empty() {
                    result.push_str(&format!("[stdout]\n{stdout}\n"));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("[stderr]\n{stderr}\n"));
                }
                Ok(result)
            }
            Ok(Err(e)) => {
                tracing::warn!(command = %command, managed_runtime, error = %e, "shell: command execution failed");
                bail!("failed to run command: {e}")
            }
            Err(_) => {
                tracing::warn!(command = %command, managed_runtime, timeout_secs = timeout.as_secs(), "shell: command timed out");
                bail!("command timed out after {:?}", timeout)
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
                if let Some(&next) = chars.peek()
                    && (next == '"' || next == '\\')
                {
                    current.push(chars.next().unwrap());
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
            None,
            None,
            None,
        )
    }

    fn make_managed_shell(level: &str) -> ShellExec {
        ShellExec::new(
            level,
            &[],
            &[],
            &["rm".to_string()],
            vec!["/etc".to_string()],
            vec![],
            5,
            None,
            Some("workspace/.venv".to_string()),
            Some("3.12".to_string()),
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

    #[test]
    fn test_managed_runtime_commands_auto_allowed() {
        let shell = make_managed_shell("supervised");
        assert!(shell.check_command_chain("python script.py").is_ok());
        assert!(shell.check_command_chain("pip install pandas").is_ok());
        assert!(shell.check_command_chain("uv pip install openpyxl").is_ok());
    }

    #[test]
    fn test_segment_uses_trusted_dir_requires_path_boundary() {
        let dir = std::env::temp_dir().join("anqclaw_test_shell_trusted_boundary");
        let trusted = dir.join("trusted");
        let sibling = dir.join("trusted-other");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let shell = ShellExec::new(
            "supervised",
            &["cat".to_string()],
            &[],
            &[],
            vec![],
            vec![trusted.to_string_lossy().to_string()],
            5,
            Some(dir.to_string_lossy().to_string()),
            None,
            None,
        );

        assert!(shell.segment_uses_trusted_dir(&format!("cat {}", trusted.join("a.txt").display())));
        assert!(!shell.segment_uses_trusted_dir(&format!("cat {}", sibling.join("a.txt").display())));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_decode_command_output_falls_back_to_gbk() {
        let bytes = [0xC9, 0xE8, 0xB1, 0xB8];
        assert_eq!(ShellExec::decode_command_output(&bytes), "设备");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_tokenize_simple_command_keeps_quoted_python_invocation() {
        let tokens = ShellExec::tokenize_simple_command(
            r#""C:\Users\lijia\.anqclaw\workspace\.venv\Scripts\python.exe" -c "print(\"ok\")""#,
        )
        .unwrap();

        assert_eq!(
            tokens,
            vec![
                "C:\\Users\\lijia\\.anqclaw\\workspace\\.venv\\Scripts\\python.exe".to_string(),
                "-c".to_string(),
                "print(\"ok\")".to_string()
            ]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_requires_shell_detects_cmd_operators() {
        assert!(ShellExec::requires_shell("dir | findstr foo"));
        assert!(!ShellExec::requires_shell(
            r#""C:\\Python312\\python.exe" script\\stats.py"#
        ));
    }
}
