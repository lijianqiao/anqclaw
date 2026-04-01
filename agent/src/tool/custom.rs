//! Custom external tool — executes an explicitly configured binary without a shell.

use anyhow::{Result, bail};
#[cfg(target_os = "windows")]
use encoding_rs::GBK;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crate::config::CustomToolConfig;

use super::Tool;
use super::tokenize::{reject_unquoted_shell_metacharacters, tokenize_quoted_args};

pub struct CustomTool {
    name: String,
    description: String,
    executable: String,
    base_args: Vec<String>,
    working_dir: PathBuf,
    blocked_dirs: Vec<String>,
    timeout: Duration,
}

impl CustomTool {
    pub fn new(
        config: &CustomToolConfig,
        working_dir: &str,
        blocked_dirs: Vec<String>,
    ) -> Result<Self> {
        let (executable, base_args) = if !config.executable.is_empty() {
            (config.executable.clone(), config.base_args.clone())
        } else {
            let tokens = Self::tokenize_argv(&config.command)?;
            let (executable, args) = tokens.split_first().ok_or_else(|| {
                anyhow::anyhow!(
                    "custom tool `{}` has an empty command / 自定义工具 `{}` 命令为空",
                    config.name,
                    config.name
                )
            })?;
            (executable.clone(), args.to_vec())
        };

        Ok(Self {
            name: config.name.clone(),
            description: config.description.clone(),
            executable,
            base_args,
            working_dir: PathBuf::from(working_dir),
            blocked_dirs,
            timeout: Duration::from_secs(config.timeout_secs as u64),
        })
    }

    fn tokenize_argv(input: &str) -> Result<Vec<String>> {
        reject_unquoted_shell_metacharacters(input)?;
        tokenize_quoted_args(input).map_err(|error| match error.to_string().as_str() {
            "unclosed quote in command / 命令中有未闭合的引号" => {
                anyhow::anyhow!(
                    "unclosed quote in custom tool command / 自定义工具命令中有未闭合的引号"
                )
            }
            "empty command / 空命令" => {
                anyhow::anyhow!("custom tool command is empty / 自定义工具命令为空")
            }
            _ => error,
        })
    }

    fn parse_runtime_args(args: &serde_json::Value) -> Result<Vec<String>> {
        match args.get("args") {
            Some(serde_json::Value::String(raw)) => Self::tokenize_argv(raw),
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .map(|value| {
                    value.as_str().map(|s| s.to_string()).ok_or_else(|| {
                        anyhow::anyhow!(
                            "custom tool args must be strings / 自定义工具参数必须是字符串"
                        )
                    })
                })
                .collect(),
            Some(_) => bail!(
                "custom tool `args` must be a string or array of strings / 自定义工具 `args` 必须是字符串或字符串数组"
            ),
            None => Ok(vec![]),
        }
    }

    fn decode_output(bytes: &[u8]) -> String {
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

    fn check_blocked_args(&self, args: &[String]) -> Result<()> {
        for arg in args {
            let candidate = Path::new(arg);
            let path = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else if arg.contains('/') || arg.contains('\\') || arg.starts_with('.') {
                self.working_dir.join(candidate)
            } else {
                continue;
            };
            super::file::check_blocked_dirs_pub(&path, &self.blocked_dirs)?;
        }
        Ok(())
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
                    "description": "Optional arguments appended to the configured executable. Prefer an array of strings; a legacy quoted string is also accepted.",
                    "oneOf": [
                        {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        {
                            "type": "string"
                        }
                    ]
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
            let extra_args = Self::parse_runtime_args(&args)?;
            self.check_blocked_args(&extra_args)?;

            let mut argv = self.base_args.clone();
            argv.extend(extra_args);

            tracing::debug!(
                tool = %self.name,
                executable = %self.executable,
                args = ?argv,
                "executing custom tool / 正在执行自定义工具"
            );

            let mut child = tokio::process::Command::new(&self.executable);
            child
                .args(&argv)
                .current_dir(&self.working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let child = child.spawn().map_err(|e| {
                anyhow::anyhow!(
                    "failed to spawn custom tool `{}`: {e} / 启动自定义工具失败",
                    self.name
                )
            })?;

            let output = tokio::time::timeout(self.timeout, child.wait_with_output())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "custom tool `{}` timed out after {}s / 自定义工具超时（{}秒）",
                        self.name,
                        self.timeout.as_secs(),
                        self.timeout.as_secs()
                    )
                })?
                .map_err(|e| anyhow::anyhow!("command execution failed / 命令执行失败: {e}"))?;

            let stdout = Self::decode_output(&output.stdout);
            let stderr = Self::decode_output(&output.stderr);

            // Limit output size to prevent blowing up LLM context
            const MAX_OUTPUT_BYTES: usize = 64 * 1024; // 64 KB

            if output.status.success() {
                let mut result = stdout;
                if !stderr.is_empty() {
                    result.push_str("\n[stderr] ");
                    result.push_str(&stderr);
                }
                if result.len() > MAX_OUTPUT_BYTES {
                    Self::truncate_output(&mut result, MAX_OUTPUT_BYTES);
                    result.push_str("\n\n[output truncated at 64 KB]");
                }
                Ok(result)
            } else {
                let code = output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                anyhow::bail!(
                    "custom tool exited with code {code} / 自定义工具退出码 {code}\nstdout: {stdout}\nstderr: {stderr}"
                )
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use std::fs;
    use std::time::Duration;

    fn make_tool(executable: String, base_args: Vec<String>, timeout_secs: u64) -> CustomTool {
        CustomTool {
            name: "test-tool".into(),
            description: "test".into(),
            executable,
            base_args,
            working_dir: std::env::temp_dir(),
            blocked_dirs: vec![],
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    #[test]
    fn test_tokenize_argv_rejects_shell_metacharacters() {
        assert!(CustomTool::tokenize_argv("python script.py && whoami").is_err());
    }

    #[test]
    fn test_parse_runtime_args_accepts_array() {
        let args = serde_json::json!({ "args": ["--flag", "value"] });
        let parsed = CustomTool::parse_runtime_args(&args).unwrap();
        assert_eq!(parsed, vec!["--flag".to_string(), "value".to_string()]);
    }

    #[test]
    fn test_check_blocked_args_rejects_blocked_path() {
        let blocked_root = std::env::temp_dir().join("anqclaw-custom-blocked");
        let tool = CustomTool {
            name: "test".into(),
            description: "test".into(),
            executable: "echo".into(),
            base_args: vec![],
            working_dir: std::env::temp_dir(),
            blocked_dirs: vec![blocked_root.to_string_lossy().into_owned()],
            timeout: Duration::from_secs(1),
        };

        let blocked_arg = blocked_root
            .join("secret.txt")
            .to_string_lossy()
            .into_owned();
        assert!(tool.check_blocked_args(&[blocked_arg]).is_err());
    }

    #[tokio::test]
    async fn test_execute_runs_real_subprocess() {
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
        let tool = make_tool(rustc, vec!["--version".into()], 5);

        let output = Tool::execute(&tool, serde_json::json!({}))
            .await
            .expect("run rustc --version");
        assert!(output.to_ascii_lowercase().contains("rustc"));
    }

    #[tokio::test]
    async fn test_execute_reports_non_zero_exit_with_stderr() {
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
        let missing_file =
            std::env::temp_dir().join(format!("anqclaw-missing-{}.rs", uuid::Uuid::new_v4()));
        let tool = make_tool(rustc, vec![missing_file.to_string_lossy().into_owned()], 5);

        let error = Tool::execute(&tool, serde_json::json!({}))
            .await
            .expect_err("missing file should fail");
        let message = error.to_string();
        assert!(message.contains("custom tool exited with code"));
        assert!(message.contains("couldn't read") || message.contains("No such file"));
    }

    #[tokio::test]
    async fn test_execute_times_out_for_long_running_process() {
        let tool = if cfg!(target_os = "windows") {
            make_tool(
                "ping".into(),
                vec!["-n".into(), "6".into(), "127.0.0.1".into()],
                1,
            )
        } else {
            make_tool("sleep".into(), vec!["2".into()], 1)
        };

        let error = Tool::execute(&tool, serde_json::json!({}))
            .await
            .expect_err("long running process should time out");
        assert!(error.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_execute_handles_large_stderr_output() {
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
        let source_path =
            std::env::temp_dir().join(format!("anqclaw-large-stderr-{}.rs", uuid::Uuid::new_v4()));

        let mut source = String::from("fn main() {\n");
        for idx in 0..250 {
            source.push_str(&format!("    let _value_{idx} = missing_symbol_{idx};\n"));
        }
        source.push_str("}\n");
        fs::write(&source_path, source).expect("write rust source");

        let tool = make_tool(rustc, vec![source_path.to_string_lossy().into_owned()], 10);
        let error = Tool::execute(&tool, serde_json::json!({}))
            .await
            .expect_err("compilation with many errors should fail");
        let message = error.to_string();

        let _ = fs::remove_file(&source_path);

        assert!(message.contains("custom tool exited with code"));
        assert!(message.contains("missing_symbol_0"));
        assert!(message.len() > 4_096);
    }
}
