//! @file
//! @author <lijianqiao>
//! @since <2026-04-01>
//! @brief 负责 shell_exec 的命令准入、执行路径判定与进程生命周期管理。
//!
//! `shell_exec` tool — runs a shell command and returns stdout + stderr.
//!
//! Safety:
//! - Three-level permission model: Readonly, Supervised, Full.
//! - Commands are checked against allow/block lists depending on level.
//! - Blocked directories are enforced in all modes.
//! - A configurable timeout kills the process if it runs too long.

mod permission;

use anyhow::{Result, bail};
#[cfg(target_os = "windows")]
use encoding_rs::GBK;
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::Tool;
use super::tokenize::tokenize_quoted_args;
const MANAGED_RUNTIME_TIMEOUT_SECS: u64 = 300;
const LOG_PREVIEW_CHARS: usize = 240;
const MAX_COMMAND_STDOUT_BYTES: usize = 32 * 1024;
const MAX_COMMAND_STDERR_BYTES: usize = 32 * 1024;
const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;

pub use permission::PermissionLevel;
pub use permission::split_command_chain;
use permission::{ALWAYS_BLOCKED, READONLY_COMMANDS, is_shell_builtin};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutionPlan {
    DirectExec { program: String, args: Vec<String> },
    RequiresShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedRuntimeCommand {
    Python,
    PipInstall,
    UvPipInstall,
}

#[derive(Debug, Default)]
struct CapturedStream {
    text: String,
    truncated: bool,
}

#[derive(Debug, Default)]
struct CapturedOutput {
    stdout: CapturedStream,
    stderr: CapturedStream,
}

#[derive(Clone)]
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

    fn is_managed_runtime_entrypoint(first_token: &str) -> bool {
        matches!(first_token, "python" | "python3" | "pip" | "pip3" | "uv")
    }

    fn managed_runtime_command_kind_from_tokens(
        tokens: &[String],
    ) -> Option<ManagedRuntimeCommand> {
        match tokens {
            [program, ..]
                if matches!(program.as_str(), "python" | "python3") && tokens.len() > 1 =>
            {
                Some(ManagedRuntimeCommand::Python)
            }
            [program, subcommand, ..]
                if matches!(program.as_str(), "pip" | "pip3") && subcommand == "install" =>
            {
                Some(ManagedRuntimeCommand::PipInstall)
            }
            [program, first_subcommand, second_subcommand, ..]
                if program == "uv"
                    && first_subcommand == "pip"
                    && second_subcommand == "install" =>
            {
                Some(ManagedRuntimeCommand::UvPipInstall)
            }
            _ => None,
        }
    }

    fn managed_runtime_command_kind(command: &str) -> Option<ManagedRuntimeCommand> {
        let tokens = Self::tokenize_command_segment(command).ok()?;
        Self::managed_runtime_command_kind_from_tokens(&tokens)
    }

    fn uses_managed_python_runtime(command: &str) -> bool {
        Self::managed_runtime_command_kind(command).is_some()
    }

    fn tokenize_command_segment(command: &str) -> Result<Vec<String>> {
        tokenize_quoted_args(command)
    }

    fn path_candidate_from_value(&self, value: &str) -> Option<PathBuf> {
        if value.is_empty() || value.starts_with('-') {
            return None;
        }

        let looks_like_path = value.starts_with('.')
            || value.starts_with('/')
            || value.starts_with("\\\\")
            || value.contains('/')
            || value.contains('\\')
            || Path::new(value).is_absolute();
        if !looks_like_path {
            return None;
        }

        let path = Path::new(value);
        Some(if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(dir) = &self.working_dir {
            dir.join(path)
        } else {
            crate::paths::anqclaw_home().join(path)
        })
    }

    fn token_candidate_paths(&self, token: &str) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if !token.contains('=')
            && let Some(candidate) = self.path_candidate_from_value(token)
        {
            candidates.push(candidate);
        }

        if token.starts_with('-')
            && let Some((_, value)) = token.split_once('=')
            && let Some(candidate) = self.path_candidate_from_value(value)
            && !candidates.contains(&candidate)
        {
            candidates.push(candidate);
        }

        candidates
    }

    fn segment_uses_trusted_dir(&self, segment: &str) -> bool {
        let Ok(tokens) = Self::tokenize_command_segment(segment) else {
            return false;
        };

        tokens
            .iter()
            .flat_map(|token| self.token_candidate_paths(token))
            .any(|candidate| crate::paths::path_is_trusted(&candidate, &self.trusted_dirs))
    }

    fn decode_command_output(bytes: &[u8]) -> String {
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
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

    fn truncate_output(text: &mut String, max_bytes: usize) {
        if text.len() <= max_bytes {
            return;
        }

        let safe_len = text
            .char_indices()
            .map(|(idx, _)| idx)
            .take_while(|idx| *idx <= max_bytes)
            .last()
            .unwrap_or(0);
        text.truncate(safe_len);
    }

    async fn capture_output_stream<R>(reader: Option<R>, max_bytes: usize) -> Result<CapturedStream>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let Some(mut reader) = reader else {
            return Ok(CapturedStream::default());
        };

        let mut captured = Vec::new();
        let mut truncated = false;
        let mut chunk = [0u8; 8192];

        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                break;
            }

            let remaining = max_bytes.saturating_sub(captured.len());
            if remaining > 0 {
                let keep = remaining.min(read);
                captured.extend_from_slice(&chunk[..keep]);
                if keep < read {
                    truncated = true;
                }
            } else {
                truncated = true;
            }
        }

        Ok(CapturedStream {
            text: Self::decode_command_output(&captured),
            truncated,
        })
    }

    async fn finish_output_capture(
        stdout_task: tokio::task::JoinHandle<Result<CapturedStream>>,
        stderr_task: tokio::task::JoinHandle<Result<CapturedStream>>,
    ) -> Result<CapturedOutput> {
        let (stdout_result, stderr_result) = tokio::join!(stdout_task, stderr_task);

        let stdout = stdout_result.map_err(|error| {
            anyhow::anyhow!("stdout capture task failed: {error} / stdout 采集任务失败: {error}")
        })??;
        let stderr = stderr_result.map_err(|error| {
            anyhow::anyhow!("stderr capture task failed: {error} / stderr 采集任务失败: {error}")
        })??;

        Ok(CapturedOutput { stdout, stderr })
    }

    fn append_stream_section(
        result: &mut String,
        label: &str,
        stream: &CapturedStream,
        max_bytes: usize,
    ) {
        if !stream.text.is_empty() {
            result.push_str(&format!("[{label}]\n{}\n", stream.text));
        }
        if stream.truncated {
            result.push_str(&format!("[{label} truncated at {max_bytes} bytes]\n"));
        }
    }

    fn format_command_result(exit_code: i32, output: &CapturedOutput) -> String {
        let mut result = format!("[exit code: {exit_code}]\n");
        Self::append_stream_section(
            &mut result,
            "stdout",
            &output.stdout,
            MAX_COMMAND_STDOUT_BYTES,
        );
        Self::append_stream_section(
            &mut result,
            "stderr",
            &output.stderr,
            MAX_COMMAND_STDERR_BYTES,
        );

        if result.len() > MAX_COMMAND_OUTPUT_BYTES {
            Self::truncate_output(&mut result, MAX_COMMAND_OUTPUT_BYTES);
            result.push_str(&format!(
                "\n[output truncated at {MAX_COMMAND_OUTPUT_BYTES} bytes]"
            ));
        }

        result
    }

    fn timeout_error_message(command: &str, timeout: Duration, output: &CapturedOutput) -> String {
        let mut message = format!(
            "command timed out after {:?}: `{}` / 命令在 {:?} 后超时: `{}`",
            timeout,
            Self::preview_for_log(command),
            timeout,
            Self::preview_for_log(command)
        );

        if !output.stdout.text.is_empty() {
            message.push_str(&format!(
                "\n[stdout preview]\n{}",
                Self::preview_for_log(&output.stdout.text)
            ));
        }
        if !output.stderr.text.is_empty() {
            message.push_str(&format!(
                "\n[stderr preview]\n{}",
                Self::preview_for_log(&output.stderr.text)
            ));
        }

        message
    }

    fn apply_process_environment(&self, cmd: &mut std::process::Command, managed_runtime: bool) {
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }

        #[cfg(not(target_os = "windows"))]
        let _ = managed_runtime;

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

        #[cfg(not(target_os = "windows"))]
        let _ = managed_runtime;

        #[cfg(target_os = "windows")]
        {
            if managed_runtime {
                cmd.env("PYTHONIOENCODING", "utf-8");
                cmd.env("PYTHONUTF8", "1");
            }
        }
    }

    fn requires_shell(command: &str) -> bool {
        let first_token = Self::normalize_first_token(command.trim());
        if is_shell_builtin(first_token) {
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
                match c {
                    '\\' => {
                        if let Some(&next) = chars.peek() {
                            #[cfg(target_os = "windows")]
                            let escaped = matches!(next, '"' | '\\' | '$' | '%' | '`');
                            #[cfg(not(target_os = "windows"))]
                            let escaped = matches!(next, '"' | '\\' | '$' | '`');
                            if escaped {
                                chars.next();
                            }
                        }
                    }
                    '"' => in_double_quote = false,
                    '\n' | '\r' | '$' | '`' => return true,
                    _ =>
                    {
                        #[cfg(target_os = "windows")]
                        if c == '%' {
                            return true;
                        }
                    }
                }
                continue;
            }

            match c {
                '\'' => in_single_quote = true,
                '"' => in_double_quote = true,
                '\n' | '\r' | '|' | '&' | ';' | '<' | '>' | '$' | '`' => return true,
                _ =>
                {
                    #[cfg(target_os = "windows")]
                    if c == '%' {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn build_execution_plan(command: &str) -> Result<ExecutionPlan> {
        if Self::requires_shell(command) {
            return Ok(ExecutionPlan::RequiresShell);
        }

        let tokens = Self::tokenize_command_segment(command)?;
        let (program, args) = tokens
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("empty command after tokenization / 分词后命令为空"))?;

        Ok(ExecutionPlan::DirectExec {
            program: program.clone(),
            args: args.to_vec(),
        })
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

    /// Check if a command's path arguments reference blocked directories.
    fn check_blocked_dirs(&self, command: &str) -> Result<()> {
        let segments = split_command_chain(command);
        for segment in &segments {
            let tokens = Self::tokenize_command_segment(segment)?;
            for token in &tokens {
                for candidate in self.token_candidate_paths(token) {
                    super::file::check_blocked_dirs_pub(&candidate, &self.blocked_dirs)?;
                }
            }
        }
        Ok(())
    }

    /// Detect dangerous argument patterns that bypass simple token blocking.
    /// Applies to ALL permission levels including Full.
    fn check_dangerous_patterns(segment: &str) -> Result<()> {
        let trimmed = segment.trim();
        let lower = trimmed.to_lowercase();
        let tokens = Self::tokenize_command_segment(trimmed).unwrap_or_default();

        // Fork bomb patterns
        if lower.contains(":(){ :|:&") || lower.contains(":(){:|:&") {
            bail!("blocked: fork bomb pattern detected / 已阻止: 检测到 fork bomb 模式");
        }

        // rm targeting root or home directory
        let first = Self::normalize_first_token(trimmed);
        if first == "rm" {
            let has_recursive = lower.contains("-r") || lower.contains("--recursive");
            let has_force = lower.contains("-f") || lower.contains("--force");
            let targets_critical = [
                " / ", " /\t", " ~", " /*", " /.", " /etc", " /usr", " /var", " /bin", " /sbin",
                " /boot",
            ]
            .iter()
            .any(|p| format!(" {trimmed} ").contains(p))
                || trimmed.ends_with(" /")
                || trimmed.ends_with(" ~");
            if has_recursive && has_force && targets_critical {
                bail!(
                    "blocked: destructive rm against critical directory / 已阻止: 对关键目录执行破坏性 rm 操作"
                );
            }
        }

        // chmod/chown on root
        if (first == "chmod" || first == "chown")
            && (lower.contains(" / ") || lower.ends_with(" /") || lower.contains(" /*"))
        {
            bail!("blocked: {first} on root directory / 已阻止: 对根目录执行 {first}");
        }

        // Direct write to block devices
        if lower.contains("> /dev/sd")
            || lower.contains("> /dev/nvme")
            || lower.contains("> /dev/hd")
        {
            bail!("blocked: direct write to block device / 已阻止: 直接写入块设备");
        }

        if first == "find"
            && tokens
                .iter()
                .any(|token| matches!(token.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
        {
            bail!(
                "blocked: find wrapper execution is not allowed / 已阻止: 不允许通过 find 包装执行其他命令"
            );
        }

        Ok(())
    }

    /// Check all sub-commands in a command chain against blocked/allowed lists.
    fn check_command_chain(&self, command: &str) -> Result<()> {
        let segments = split_command_chain(command);
        for segment in &segments {
            let first_token = Self::normalize_first_token(segment);
            let managed_runtime_kind = if self.venv_path.is_some() {
                Self::managed_runtime_command_kind(segment)
            } else {
                None
            };
            let tokens = Self::tokenize_command_segment(segment).unwrap_or_default();

            // Check blocked commands (applies to ALL modes)
            if self.blocked.contains(first_token) {
                bail!(
                    "command `{first_token}` is blocked for safety reasons / 命令 `{first_token}` 因安全原因被阻止"
                );
            }

            // Check if the segment starts with any blocked pattern
            for blocked in &self.blocked {
                if segment.starts_with(blocked.as_str()) {
                    bail!(
                        "command pattern `{blocked}` is blocked for safety reasons / 命令模式 `{blocked}` 因安全原因被阻止"
                    );
                }
            }

            // Check dangerous argument patterns (applies to ALL modes)
            Self::check_dangerous_patterns(segment)?;

            self.check_blocked_dirs(segment)?;

            if matches!(first_token, "env" | "printenv") && tokens.len() > 1 {
                bail!(
                    "command `{first_token}` cannot wrap another command in restricted modes / 命令 `{first_token}` 在受限模式下不能包装其他命令"
                );
            }

            if self.venv_path.is_some()
                && Self::is_managed_runtime_entrypoint(first_token)
                && managed_runtime_kind.is_none()
            {
                bail!(
                    "managed runtime command `{segment}` is not allowed; supported forms are `python <args>`, `python3 <args>`, `pip install <pkg>`, `pip3 install <pkg>`, or `uv pip install <pkg>` / 托管运行时命令 `{segment}` 不被允许；仅支持 `python <args>`、`python3 <args>`、`pip install <pkg>`、`pip3 install <pkg>` 或 `uv pip install <pkg>`"
                );
            }

            // Permission check — per-segment, not whole command
            let in_trusted =
                !self.trusted_dirs.is_empty() && self.segment_uses_trusted_dir(segment);

            match self.permission_level {
                PermissionLevel::Readonly | PermissionLevel::Supervised => {
                    if !in_trusted
                        && !self.allowed.contains(first_token)
                        && managed_runtime_kind.is_none()
                    {
                        bail!(
                            "command `{first_token}` is not allowed in {:?} mode. Allowed: {:?} / 命令 `{first_token}` 在 {:?} 模式下不被允许。允许的命令: {:?}",
                            self.permission_level,
                            self.allowed,
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

    fn missing_uv_error() -> anyhow::Error {
        anyhow::anyhow!(
            "managed Python runtime requires a locally installed uv; automatic uv bootstrap is disabled. Install uv manually from https://docs.astral.sh/uv/getting-started/installation/ and retry, or disable venv-managed package installation. / 托管式 Python 运行时需要本地已安装 uv；自动引导安装 uv 的功能已被禁用。请从 https://docs.astral.sh/uv/getting-started/installation/ 手动安装 uv 后重试，或禁用由 venv 托管的包安装功能。"
        )
    }

    fn install_uv() -> Result<PathBuf> {
        tracing::warn!(
            "managed runtime: automatic uv bootstrap is disabled / 托管运行时: 已禁用自动 uv 自举"
        );
        Err(Self::missing_uv_error())
    }

    fn ensure_uv_available(&self) -> Result<PathBuf> {
        if let Some(path) = Self::existing_uv_path() {
            tracing::info!(path = %path.display(), "managed runtime: using existing uv / 托管运行时: 使用现有 uv");
            return Ok(path);
        }

        tracing::warn!(
            "managed runtime: uv not found and automatic bootstrap is disabled / 托管运行时: 未找到 uv，且已禁用自动自举"
        );
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

    fn ensure_managed_python_runtime_blocking(&self) -> Result<PathBuf> {
        let Some(venv_abs) = self.managed_venv_path() else {
            bail!(
                "managed Python runtime requested without a configured venv path / 请求托管 Python 运行时但未配置 venv 路径"
            )
        };

        let python_bin = Self::managed_python_binary(&venv_abs);
        if python_bin.exists() {
            tracing::info!(venv = %venv_abs.display(), python = %python_bin.display(), "managed runtime: existing venv is ready / 托管运行时: 现有 venv 已准备好");
            return Ok(venv_abs);
        }

        if let Some(parent) = venv_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let uv_path = self.ensure_uv_available()?;
        let version = self.preferred_python_version().to_string();

        tracing::info!(venv = %venv_abs.display(), python = %version, uv = %uv_path.display(), "managed runtime: installing Python with uv / 托管运行时: 使用 uv 安装 Python");
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
                    "managed runtime: Python installation step completed / 托管运行时: Python 安装步骤完成"
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
                    "managed runtime: Python installation step failed / 托管运行时: Python 安装步骤失败"
                );
                bail!(
                    "failed to install managed Python {} with uv / 使用 uv 安装托管 Python {} 失败",
                    version,
                    version
                )
            }
            Err(error) => {
                tracing::warn!(error = %error, python = %version, "managed runtime: failed to launch `uv python install` / 托管运行时: 启动 `uv python install` 失败");
                bail!(
                    "failed to install managed Python {} with uv / 使用 uv 安装托管 Python {} 失败",
                    version,
                    version
                )
            }
        }

        tracing::info!(venv = %venv_abs.display(), python = %version, "managed runtime: creating venv / 托管运行时: 创建 venv");
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
                    "managed runtime: venv creation step completed / 托管运行时: venv 创建步骤完成"
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
                    "managed runtime: venv creation step failed / 托管运行时: venv 创建步骤失败"
                );
                bail!(
                    "failed to create managed venv at {} / 在 {} 创建托管 venv 失败",
                    venv_abs.display(),
                    venv_abs.display()
                )
            }
            Err(error) => {
                tracing::warn!(error = %error, venv = %venv_abs.display(), "managed runtime: failed to launch `uv venv` / 托管运行时: 启动 `uv venv` 失败");
                bail!(
                    "failed to create managed venv at {} / 在 {} 创建托管 venv 失败",
                    venv_abs.display(),
                    venv_abs.display()
                )
            }
        }

        if !python_bin.exists() {
            bail!(
                "managed Python bootstrap finished but interpreter is missing at {} / 托管 Python 引导完成但解释器在 {} 处缺失",
                python_bin.display(),
                python_bin.display()
            )
        }

        tracing::info!(venv = %venv_abs.display(), python = %version, "managed Python runtime bootstrapped / 托管 Python 运行时已引导完成");
        Ok(venv_abs)
    }

    async fn ensure_managed_python_runtime(&self) -> Result<PathBuf> {
        let shell = self.clone();
        tokio::task::spawn_blocking(move || shell.ensure_managed_python_runtime_blocking())
            .await
            .map_err(|error| anyhow::anyhow!("managed runtime bootstrap task failed: {error} / 托管运行时引导任务失败: {error}"))?
    }

    /// Rewrite a command to run inside the venv if it's a pip/uv install or
    /// python invocation. Replaces the bare `python`/`pip` with the venv's
    /// absolute path — no activate needed, works on all platforms.
    async fn rewrite_for_venv(&self, command: &str) -> Result<(String, bool)> {
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

        let venv_abs = self.ensure_managed_python_runtime().await?;
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
            .ok_or_else(|| anyhow::anyhow!("missing `command` parameter / 缺少 `command` 参数"))?;

        // Check all sub-commands in the chain (pipes, &&, || etc.)
        self.check_command_chain(command)?;

        // Rewrite pip/python commands to use venv if configured
        let managed_runtime =
            self.venv_path.is_some() && Self::uses_managed_python_runtime(command);
        let (command, _venv_created) = self.rewrite_for_venv(command).await?;
        let command = command.as_str();
        let timeout = if managed_runtime {
            self.timeout
                .max(Duration::from_secs(MANAGED_RUNTIME_TIMEOUT_SECS))
        } else {
            self.timeout
        };
        let execution_plan = Self::build_execution_plan(command)?;
        let using_shell = matches!(execution_plan, ExecutionPlan::RequiresShell);

        tracing::info!(
            command = %command,
            managed_runtime,
            using_shell,
            cwd = ?self.working_dir.as_ref().map(|p| p.display().to_string()),
            timeout_secs = timeout.as_secs(),
            "shell: starting command"
        );

        let mut child = match execution_plan {
            ExecutionPlan::DirectExec { program, args } => {
                let mut cmd = tokio::process::Command::new(&program);
                cmd.args(&args);
                self.apply_async_process_environment(&mut cmd, managed_runtime);
                cmd.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?
            }
            ExecutionPlan::RequiresShell => match self.permission_level {
                PermissionLevel::Readonly | PermissionLevel::Supervised => {
                    bail!(
                        "command requires shell syntax and is not allowed in {:?} mode / 命令需要 shell 语法，在 {:?} 模式下不被允许",
                        self.permission_level,
                        self.permission_level
                    );
                }
                PermissionLevel::Full => {
                    #[cfg(target_os = "windows")]
                    {
                        let mut cmd = tokio::process::Command::new("cmd");
                        cmd.args(["/S", "/C", &Self::wrap_for_cmd(command)]);
                        self.apply_async_process_environment(&mut cmd, managed_runtime);
                        cmd.stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()?
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
                }
            },
        };

        let stdout_task = tokio::spawn(Self::capture_output_stream(
            child.stdout.take(),
            MAX_COMMAND_STDOUT_BYTES,
        ));
        let stderr_task = tokio::spawn(Self::capture_output_stream(
            child.stderr.take(),
            MAX_COMMAND_STDERR_BYTES,
        ));

        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => {
                let output = Self::finish_output_capture(stdout_task, stderr_task).await?;
                let exit_code = status.code().unwrap_or(-1);
                if exit_code == 0 {
                    tracing::info!(
                        command = %command,
                        managed_runtime,
                        exit_code,
                        stdout_bytes = output.stdout.text.len(),
                        stderr_bytes = output.stderr.text.len(),
                        stdout_truncated = output.stdout.truncated,
                        stderr_truncated = output.stderr.truncated,
                        "shell: command finished"
                    );
                } else {
                    tracing::warn!(
                        command = %command,
                        managed_runtime,
                        exit_code,
                        stdout_bytes = output.stdout.text.len(),
                        stderr_bytes = output.stderr.text.len(),
                        stdout_truncated = output.stdout.truncated,
                        stderr_truncated = output.stderr.truncated,
                        stderr_preview = %Self::preview_for_log(&output.stderr.text),
                        "shell: command finished with non-zero exit / shell: 命令以非零退出码结束"
                    );
                }
                Ok(Self::format_command_result(exit_code, &output))
            }
            Ok(Err(e)) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let _ = Self::finish_output_capture(stdout_task, stderr_task).await;
                tracing::warn!(command = %command, managed_runtime, error = %e, "shell: command execution failed / shell: 命令执行失败");
                bail!("failed to run command: {e} / 运行命令失败: {e}")
            }
            Err(_) => {
                if let Err(error) = child.start_kill() {
                    tracing::warn!(command = %command, managed_runtime, error = %error, "shell: failed to kill timed out command / shell: 终止超时命令失败");
                }
                let _ = child.wait().await;
                let output = match Self::finish_output_capture(stdout_task, stderr_task).await {
                    Ok(output) => output,
                    Err(error) => {
                        tracing::warn!(command = %command, managed_runtime, error = %error, "shell: failed to capture output after timeout / shell: 超时后采集输出失败");
                        CapturedOutput::default()
                    }
                };
                tracing::warn!(
                    command = %command,
                    managed_runtime,
                    timeout_secs = timeout.as_secs(),
                    stdout_preview = %Self::preview_for_log(&output.stdout.text),
                    stderr_preview = %Self::preview_for_log(&output.stderr.text),
                    stdout_truncated = output.stdout.truncated,
                    stderr_truncated = output.stderr.truncated,
                    "shell: command timed out / shell: 命令超时"
                );
                bail!("{}", Self::timeout_error_message(command, timeout, &output))
            }
        }
    }
}

impl Tool for ShellExec {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its stdout, stderr, and exit code. Commands are subject to permission level restrictions. / 执行 shell 命令并返回其 stdout、stderr 和退出码。命令受权限级别限制。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute / 要执行的 shell 命令"
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_shell(level: &str) -> ShellExec {
        make_shell_with_timeout(level, 5)
    }

    fn make_shell_with_timeout(level: &str, timeout_secs: u32) -> ShellExec {
        ShellExec::new(
            level,
            &[],
            &[],
            &["rm".to_string()],
            vec!["/etc".to_string()],
            vec![],
            timeout_secs,
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

    fn make_shell_with_blocked_dir(
        level: &str,
        working_dir: &Path,
        blocked_dir: &str,
    ) -> ShellExec {
        ShellExec::new(
            level,
            &[],
            &[],
            &[],
            vec![blocked_dir.to_string()],
            vec![],
            5,
            Some(working_dir.to_string_lossy().to_string()),
            None,
            None,
        )
    }

    #[cfg(target_os = "windows")]
    fn large_output_command() -> String {
        r#"powershell -NoProfile -Command '$i=0; while ($i -lt 1500) { [Console]::Out.WriteLine("0123456789ABCDEF0123456789ABCDEF"); $i++ }'"#.to_string()
    }

    #[cfg(not(target_os = "windows"))]
    fn large_output_command() -> String {
        "i=0; while [ $i -lt 2500 ]; do printf '0123456789ABCDEF0123456789ABCDEF\\n'; i=$((i+1)); done".to_string()
    }

    #[cfg(target_os = "windows")]
    fn noisy_both_streams_command() -> String {
        r#"powershell -NoProfile -Command '$i=0; while ($i -lt 1500) { [Console]::Out.WriteLine("stdout-line-$i"); [Console]::Error.WriteLine("stderr-line-$i"); $i++ }'"#.to_string()
    }

    #[cfg(not(target_os = "windows"))]
    fn noisy_both_streams_command() -> String {
        "i=0; while [ $i -lt 2500 ]; do echo stdout-line-$i; echo stderr-line-$i 1>&2; i=$((i+1)); done".to_string()
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for tests / 时钟应该足够单调以供测试使用")
            .as_nanos();
        std::env::temp_dir().join(format!("anqclaw_{name}_{}_{}", std::process::id(), nonce))
    }

    #[cfg(target_os = "windows")]
    fn timeout_marker_command(path: &Path) -> String {
        let escaped = path.display().to_string().replace('\'', "''");
        format!(
            "powershell -NoProfile -Command \"Start-Sleep -Seconds 2; Set-Content -Path '{escaped}' -Value done\""
        )
    }

    #[cfg(not(target_os = "windows"))]
    fn timeout_marker_command(path: &Path) -> String {
        format!("sh -c 'sleep 2; printf done > \"{}\"'", path.display())
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
    async fn test_readonly_allows_direct_exec_command() {
        let shell = make_shell("readonly");
        let args = serde_json::json!({ "command": "hostname" });
        let result = shell.do_execute(args).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("[exit code: 0]"));
    }

    #[tokio::test]
    async fn test_readonly_rejects_shell_pipeline() {
        let shell = make_shell("readonly");
        let args = serde_json::json!({ "command": "hostname | sort" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("requires shell syntax")
        );
    }

    #[tokio::test]
    async fn test_supervised_rejects_shell_redirection() {
        let shell = make_shell("supervised");
        let args = serde_json::json!({ "command": "hostname > out.txt" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("requires shell syntax")
        );
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
        let dir = unique_temp_path("shell_blocked_execute");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();

        let shell = make_shell_with_blocked_dir("full", &dir, ".ssh");
        let args = serde_json::json!({ "command": "hostname .ssh/id_rsa" });
        let result = shell.do_execute(args).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("blocked directory")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_blocked_dir_matching_requires_component_boundary() {
        let dir = unique_temp_path("shell_blocked_boundary");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        std::fs::create_dir_all(dir.join(".ssh_backup")).unwrap();

        let shell = make_shell_with_blocked_dir("full", &dir, ".ssh");

        assert!(shell.check_command_chain("cat .ssh/id_rsa").is_err());
        assert!(shell.check_command_chain("cat .ssh_backup/id_rsa").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_blocked_dir_matching_checks_option_values_only_when_path_like() {
        let dir = unique_temp_path("shell_blocked_option");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();

        let shell = make_shell_with_blocked_dir("full", &dir, ".ssh");

        let error = shell
            .check_command_chain("tool --output=.ssh/id_rsa")
            .expect_err("path-like option value should be blocked / 路径形态参数值应被拦截");
        assert!(error.to_string().contains("blocked directory"));

        assert!(shell.check_command_chain("echo status=.ssh_backup").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
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
        #[cfg(target_os = "windows")]
        let command = "echo hello | findstr hello";
        #[cfg(not(target_os = "windows"))]
        let command = "echo hello | grep hello";
        assert!(shell.check_command_chain(command).is_ok());
    }

    #[tokio::test]
    async fn test_full_allows_shell_pipeline_execution() {
        let shell = make_shell_with_timeout("full", 15);
        #[cfg(target_os = "windows")]
        let command = "echo hello | findstr hello";
        #[cfg(not(target_os = "windows"))]
        let command = "printf 'hello\\n' | grep hello";

        let result = shell
            .do_execute(serde_json::json!({ "command": command }))
            .await
            .unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_large_output_is_truncated() {
        let shell = make_shell_with_timeout("full", 25);
        let result = shell
            .do_execute(serde_json::json!({ "command": large_output_command() }))
            .await
            .unwrap();

        assert!(result.contains("[stdout truncated at"));
        assert!(result.len() <= MAX_COMMAND_OUTPUT_BYTES + 128);
    }

    #[tokio::test]
    async fn test_large_stdout_and_stderr_are_drained_without_deadlock() {
        let shell = make_shell_with_timeout("full", 25);
        let result = shell
            .do_execute(serde_json::json!({ "command": noisy_both_streams_command() }))
            .await
            .unwrap();

        assert!(result.contains("stdout-line"));
        assert!(result.contains("[stderr]"));
        assert!(result.contains("stderr-line"));
    }

    #[tokio::test]
    async fn test_timeout_kills_child_process_before_side_effect() {
        let shell = make_shell_with_timeout("full", 1);
        let dir = unique_temp_path("shell_timeout");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("marker.txt");

        let result = shell
            .do_execute(serde_json::json!({ "command": timeout_marker_command(&marker) }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out after"));

        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !marker.exists(),
            "timed out child should be killed before writing marker / 超时子进程应在写入标记前被终止"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_execution_plan_uses_direct_exec_for_plain_argv() {
        let plan = ShellExec::build_execution_plan("hostname").unwrap();
        assert_eq!(
            plan,
            ExecutionPlan::DirectExec {
                program: "hostname".to_string(),
                args: vec![]
            }
        );
    }

    #[test]
    fn test_build_execution_plan_requires_shell_for_metacharacters() {
        let mut commands = vec![
            "hostname | sort",
            "hostname > out.txt",
            "hostname < in.txt",
            "printf foo\nbar",
            "echo `whoami`",
        ];

        #[cfg(target_os = "windows")]
        commands.push("echo %USERPROFILE%");
        #[cfg(not(target_os = "windows"))]
        commands.push("echo $HOME");

        for command in commands {
            assert_eq!(
                ShellExec::build_execution_plan(command).unwrap(),
                ExecutionPlan::RequiresShell,
                "expected `{command}` to require shell"
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_build_execution_plan_marks_cmd_builtin_as_requires_shell() {
        assert_eq!(
            ShellExec::build_execution_plan("echo hello").unwrap(),
            ExecutionPlan::RequiresShell
        );
    }

    #[test]
    fn test_managed_runtime_commands_allow_only_expected_shapes() {
        let shell = make_managed_shell("supervised");
        assert!(shell.check_command_chain("python script.py").is_ok());
        assert!(shell.check_command_chain("pip install pandas").is_ok());
        assert!(shell.check_command_chain("uv pip install openpyxl").is_ok());
        assert!(shell.check_command_chain("uv run script.py").is_err());
        assert!(shell.check_command_chain("pip list").is_err());
    }

    #[test]
    fn test_find_exec_is_rejected() {
        let shell = make_shell("supervised");
        let error = shell
            .check_command_chain("find . -exec whoami {} +")
            .expect_err("find -exec should be rejected / 应拒绝 find -exec");
        assert!(error.to_string().contains("find wrapper execution"));
    }

    #[test]
    fn test_env_wrapper_is_rejected() {
        let shell = make_shell("supervised");
        let error = shell
            .check_command_chain("env FOO=bar hostname")
            .expect_err("env wrapper should be rejected / 应拒绝 env 包装器");
        assert!(error.to_string().contains("cannot wrap another command"));
    }

    #[test]
    fn test_install_uv_is_fail_closed() {
        let error = ShellExec::install_uv()
            .expect_err("automatic uv bootstrap should stay disabled / 应禁用自动 uv 自举");
        let message = error.to_string();
        assert!(message.contains("automatic uv bootstrap is disabled"));
        assert!(message.contains("docs.astral.sh/uv/getting-started/installation"));
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

        assert!(
            shell.segment_uses_trusted_dir(&format!("cat {}", trusted.join("a.txt").display()))
        );
        assert!(
            !shell.segment_uses_trusted_dir(&format!("cat {}", sibling.join("a.txt").display()))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_blocked_dir_matching_is_case_insensitive_on_windows() {
        let dir = unique_temp_path("shell_blocked_case");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".SSH")).unwrap();

        let shell = make_shell_with_blocked_dir("full", &dir, ".ssh");

        assert!(shell.check_command_chain("cat .SSH/id_rsa").is_err());

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
    fn test_build_execution_plan_keeps_quoted_python_invocation() {
        let plan = ShellExec::build_execution_plan(
            r#""C:\Users\lijia\.anqclaw\workspace\.venv\Scripts\python.exe" -c "print(\"ok\")""#,
        )
        .unwrap();

        assert_eq!(
            plan,
            ExecutionPlan::DirectExec {
                program: "C:\\Users\\lijia\\.anqclaw\\workspace\\.venv\\Scripts\\python.exe"
                    .to_string(),
                args: vec!["-c".to_string(), "print(\"ok\")".to_string()]
            }
        );
    }

    #[test]
    fn test_requires_shell_detects_operators() {
        assert!(ShellExec::requires_shell("hostname | sort"));
        assert!(!ShellExec::requires_shell(
            r#""C:\\Python312\\python.exe" script\\stats.py"#
        ));
    }
}
