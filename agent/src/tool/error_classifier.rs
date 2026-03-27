//! Structured error classification for tool outputs.
//!
//! Classifies tool errors (exit codes, stderr patterns) into structured types
//! with actionable hints. Appended to tool results so the LLM can fix issues faster.

use crate::agent::probe::EnvironmentProbe;

/// Error classification categories.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolErrorKind {
    CommandNotFound { command: String },
    ModuleNotFound { module: String, language: String },
    PermissionDenied,
    Timeout,
    SyntaxError { language: String },
    NetworkError,
    FileNotFound { path: String },
    DiskFull,
    Unknown,
}

/// Classification result with optional hint.
#[derive(Debug)]
pub struct ErrorClassification {
    pub kind: ToolErrorKind,
    pub hint: Option<String>,
}

/// Parse `[exit code: N]` from tool output, returning the exit code.
pub fn parse_exit_code(output: &str) -> Option<i32> {
    let marker = "[exit code: ";
    let start = output.rfind(marker)?;
    let rest = &output[start + marker.len()..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

/// Classify a tool error from its output and exit code.
pub fn classify_error(
    _tool_name: &str,
    output: &str,
    exit_code: Option<i32>,
    env: &EnvironmentProbe,
) -> ErrorClassification {
    // 1. Command not found (exit_code 127 on Unix, 9009 on Windows)
    if exit_code == Some(127)
        || exit_code == Some(9009)
        || output.contains("command not found")
        || output.contains("is not recognized as an internal or external command")
    {
        let command = extract_missing_command(output);
        let hint = suggest_install_command(&command, env);
        return ErrorClassification {
            kind: ToolErrorKind::CommandNotFound { command },
            hint,
        };
    }

    // 2. Python ModuleNotFoundError
    if output.contains("ModuleNotFoundError") || output.contains("No module named") {
        let module = extract_module_name(output);
        let hint = if env.has("uv") {
            Some(format!("You may install it: `uv pip install {module}`"))
        } else if env.has("pip3") {
            Some(format!("You may install it: `pip3 install {module}`"))
        } else if env.has("pip") {
            Some(format!("You may install it: `pip install {module}`"))
        } else {
            Some("pip is not available. Inform the user to install the package.".into())
        };
        return ErrorClassification {
            kind: ToolErrorKind::ModuleNotFound {
                module,
                language: "python".into(),
            },
            hint,
        };
    }

    // 3. Node.js module not found
    if output.contains("Cannot find module") || output.contains("MODULE_NOT_FOUND") {
        let module = extract_node_module_name(output);
        let hint = if env.has("npm") {
            Some(format!("You may install it: `npm install {module}`"))
        } else {
            Some("npm is not available.".into())
        };
        return ErrorClassification {
            kind: ToolErrorKind::ModuleNotFound {
                module,
                language: "node".into(),
            },
            hint,
        };
    }

    // 4. Permission denied
    if output.contains("Permission denied")
        || output.contains("EACCES")
        || output.contains("Access is denied")
    {
        return ErrorClassification {
            kind: ToolErrorKind::PermissionDenied,
            hint: Some("Try with appropriate permissions or a different approach.".into()),
        };
    }

    // 5. Syntax error
    if output.contains("SyntaxError") || output.contains("IndentationError") {
        return ErrorClassification {
            kind: ToolErrorKind::SyntaxError {
                language: "python".into(),
            },
            hint: Some("Check the generated code for syntax issues.".into()),
        };
    }

    // 6. Timeout
    if output.contains("command timed out") || output.contains("timed out after") {
        return ErrorClassification {
            kind: ToolErrorKind::Timeout,
            hint: Some("The command exceeded the timeout limit. Try a faster approach or increase the timeout.".into()),
        };
    }

    // 7. File not found
    if output.contains("No such file or directory")
        || output.contains("FileNotFoundError")
        || output.contains("The system cannot find the")
    {
        let path = extract_file_path(output);
        return ErrorClassification {
            kind: ToolErrorKind::FileNotFound { path },
            hint: None,
        };
    }

    // 8. Network error
    if output.contains("ConnectionRefusedError")
        || output.contains("ECONNREFUSED")
        || output.contains("Could not resolve host")
        || output.contains("Failed to fetch")
        || output.contains("tls handshake")
        || output.contains("certificate verify failed")
        || output.contains("Temporary failure in name resolution")
    {
        return ErrorClassification {
            kind: ToolErrorKind::NetworkError,
            hint: Some(
                "Check network connectivity, proxy/TLS settings, or install the dependency manually before retrying."
                    .into(),
            ),
        };
    }

    // 8. Disk full
    if output.contains("No space left on device") || output.contains("ENOSPC") {
        return ErrorClassification {
            kind: ToolErrorKind::DiskFull,
            hint: Some("Disk is full. Free up space before retrying.".into()),
        };
    }

    // 9. Default
    ErrorClassification {
        kind: ToolErrorKind::Unknown,
        hint: None,
    }
}

/// Format classification as an annotation appended to tool output.
pub fn format_error_annotation(classification: &ErrorClassification) -> String {
    let kind_label = match &classification.kind {
        ToolErrorKind::CommandNotFound { command } => format!("command_not_found:{command}"),
        ToolErrorKind::ModuleNotFound { module, language } => {
            format!("module_not_found:{language}:{module}")
        }
        ToolErrorKind::PermissionDenied => "permission_denied".into(),
        ToolErrorKind::Timeout => "timeout".into(),
        ToolErrorKind::SyntaxError { language } => format!("syntax_error:{language}"),
        ToolErrorKind::NetworkError => "network_error".into(),
        ToolErrorKind::FileNotFound { path } => format!("file_not_found:{path}"),
        ToolErrorKind::DiskFull => "disk_full".into(),
        ToolErrorKind::Unknown => "unknown".into(),
    };

    let mut s = format!("\n\n[error_type: {kind_label}]");
    if let Some(hint) = &classification.hint {
        s += &format!("\n[hint: {hint}]");
    }
    s
}

// ─── Helper extraction functions ─────────────────────────────────────────────

/// Extract command name from "xxx: command not found" or "'xxx' is not recognized".
fn extract_missing_command(output: &str) -> String {
    // Unix: "bash: python3: command not found" → extract "python3"
    if let Some(pos) = output.find(": command not found") {
        let before = &output[..pos];
        // The command is the segment after the last ':'
        // e.g. "bash: python3" → split on ':' → last = " python3"
        if let Some(cmd) = before.rsplit_once(':') {
            return cmd.1.trim().to_string();
        }
        // Or take from the last newline
        if let Some(cmd) = before.rsplit_once('\n') {
            return cmd.1.trim().to_string();
        }
        return before.trim().to_string();
    }
    // Windows: "'python3' is not recognized"
    if let Some(pos) = output.find("is not recognized") {
        let before = &output[..pos];
        // Extract between first pair of quotes: 'xxx'
        if let Some(start) = before.find('\'') {
            let after_start = &before[start + 1..];
            if let Some(end) = after_start.find('\'') {
                let name = after_start[..end].trim().to_string();
                if !name.is_empty() {
                    return name;
                }
            }
        }
        // Or just take the last word
        return before
            .split_whitespace()
            .last()
            .unwrap_or("unknown")
            .to_string();
    }
    "unknown".to_string()
}

/// Extract Python module name from "No module named 'xxx'" or "ModuleNotFoundError: No module named 'xxx.yyy'".
fn extract_module_name(output: &str) -> String {
    // Look for quoted module name after "No module named"
    if let Some(pos) = output.find("No module named") {
        let rest = &output[pos + "No module named".len()..];
        // Try extracting from quotes: 'xxx' or "xxx"
        if let Some(name) = extract_quoted(rest) {
            // Return top-level package (e.g. "openpyxl" from "openpyxl.utils")
            return name.split('.').next().unwrap_or(&name).to_string();
        }
    }
    "unknown".to_string()
}

/// Extract Node.js module name from "Cannot find module 'xxx'".
fn extract_node_module_name(output: &str) -> String {
    if let Some(pos) = output.find("Cannot find module") {
        let rest = &output[pos + "Cannot find module".len()..];
        if let Some(name) = extract_quoted(rest) {
            return name;
        }
    }
    "unknown".to_string()
}

/// Extract a file path from error messages.
fn extract_file_path(output: &str) -> String {
    // Python: FileNotFoundError: [Errno 2] No such file or directory: 'path'
    if let Some(pos) = output.find("No such file or directory") {
        let rest = &output[pos..];
        if let Some(name) = extract_quoted(rest) {
            return name;
        }
    }
    // Windows: The system cannot find the file specified.
    // Generic: just return "unknown"
    "unknown".to_string()
}

/// Extract first single- or double-quoted string from text.
fn extract_quoted(text: &str) -> Option<String> {
    for quote in ['\'', '"'] {
        if let Some(start) = text.find(quote) {
            let after = &text[start + 1..];
            if let Some(end) = after.find(quote) {
                let s = after[..end].trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Suggest an install command for a missing binary.
fn suggest_install_command(command: &str, env: &EnvironmentProbe) -> Option<String> {
    match command {
        "python3" | "python" => {
            if env.has("uv") {
                Some("uv python install".into())
            } else {
                Some("Install Python from https://python.org or via system package manager.".into())
            }
        }
        "pip3" | "pip" => {
            if env.has("python3") || env.has("python") {
                Some("python3 -m ensurepip --upgrade".into())
            } else {
                Some("Install Python first (pip is included).".into())
            }
        }
        "node" | "npm" => Some("Install Node.js from https://nodejs.org".into()),
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::probe::{BinaryInfo, EnvironmentProbe};
    use std::collections::HashMap;

    fn make_probe(entries: &[(&str, bool)]) -> EnvironmentProbe {
        let mut binaries = HashMap::new();
        for (name, available) in entries {
            binaries.insert(
                name.to_string(),
                BinaryInfo {
                    available: *available,
                    version: None,
                    path: None,
                },
            );
        }
        EnvironmentProbe { binaries }
    }

    fn empty_probe() -> EnvironmentProbe {
        make_probe(&[])
    }

    #[test]
    fn test_classify_command_not_found_unix() {
        let output = "bash: python3: command not found\n[exit code: 127]";
        let env = empty_probe();
        let c = classify_error("shell_exec", output, Some(127), &env);
        assert_eq!(
            c.kind,
            ToolErrorKind::CommandNotFound {
                command: "python3".into()
            }
        );
    }

    #[test]
    fn test_classify_command_not_found_windows() {
        let output =
            "'python3' is not recognized as an internal or external command\n[exit code: 9009]";
        let env = empty_probe();
        let c = classify_error("shell_exec", output, Some(9009), &env);
        assert_eq!(
            c.kind,
            ToolErrorKind::CommandNotFound {
                command: "python3".into()
            }
        );
    }

    #[test]
    fn test_classify_module_not_found_python() {
        let output = "Traceback (most recent call last):\n  File \"script.py\", line 1\nModuleNotFoundError: No module named 'pandas'\n[exit code: 1]";
        let env = make_probe(&[("pip3", true)]);
        let c = classify_error("shell_exec", output, Some(1), &env);
        assert_eq!(
            c.kind,
            ToolErrorKind::ModuleNotFound {
                module: "pandas".into(),
                language: "python".into()
            }
        );
        assert!(c.hint.as_ref().unwrap().contains("pip3 install pandas"));
    }

    #[test]
    fn test_classify_module_not_found_node() {
        let output = "Error: Cannot find module 'express'\n[exit code: 1]";
        let env = make_probe(&[("npm", true)]);
        let c = classify_error("shell_exec", output, Some(1), &env);
        assert_eq!(
            c.kind,
            ToolErrorKind::ModuleNotFound {
                module: "express".into(),
                language: "node".into()
            }
        );
        assert!(c.hint.as_ref().unwrap().contains("npm install express"));
    }

    #[test]
    fn test_classify_permission_denied() {
        let output = "bash: /usr/sbin/something: Permission denied\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(c.kind, ToolErrorKind::PermissionDenied);
    }

    #[test]
    fn test_classify_syntax_error() {
        let output = "  File \"script.py\", line 3\n    print(foo\n         ^\nSyntaxError: invalid syntax\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(
            c.kind,
            ToolErrorKind::SyntaxError {
                language: "python".into()
            }
        );
    }

    #[test]
    fn test_classify_file_not_found() {
        let output =
            "FileNotFoundError: [Errno 2] No such file or directory: 'data.csv'\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(
            c.kind,
            ToolErrorKind::FileNotFound {
                path: "data.csv".into()
            }
        );
    }

    #[test]
    fn test_classify_network_error() {
        let output = "ConnectionRefusedError: [Errno 111] Connection refused\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(c.kind, ToolErrorKind::NetworkError);
        assert!(c.hint.as_ref().unwrap().contains("network"));
    }

    #[test]
    fn test_classify_uv_tls_fetch_failure_as_network_error() {
        let output = "error: Request failed after 3 retries\n  Caused by: Failed to fetch: `https://pypi.org/simple/pandas/`\n  Caused by: client error (Connect)\n  Caused by: tls handshake eof\n[exit code: 2]";
        let c = classify_error("shell_exec", output, Some(2), &empty_probe());
        assert_eq!(c.kind, ToolErrorKind::NetworkError);
        assert!(c.hint.as_ref().unwrap().contains("TLS"));
    }

    #[test]
    fn test_classify_disk_full() {
        let output = "OSError: [Errno 28] No space left on device\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(c.kind, ToolErrorKind::DiskFull);
    }

    #[test]
    fn test_classify_timeout() {
        let output = "command timed out after 30s";
        let c = classify_error("shell_exec", output, None, &empty_probe());
        assert_eq!(c.kind, ToolErrorKind::Timeout);
        assert!(c.hint.as_ref().unwrap().contains("timeout"));
    }

    #[test]
    fn test_classify_unknown() {
        let output = "some random error output\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(c.kind, ToolErrorKind::Unknown);
        assert!(c.hint.is_none());
    }

    #[test]
    fn test_parse_exit_code() {
        assert_eq!(parse_exit_code("output\n[exit code: 1]"), Some(1));
        assert_eq!(parse_exit_code("output\n[exit code: 127]"), Some(127));
        assert_eq!(parse_exit_code("output\n[exit code: 0]"), Some(0));
        assert_eq!(parse_exit_code("no exit code here"), None);
    }

    #[test]
    fn test_format_error_annotation() {
        let c = ErrorClassification {
            kind: ToolErrorKind::ModuleNotFound {
                module: "pandas".into(),
                language: "python".into(),
            },
            hint: Some("pip3 install pandas".into()),
        };
        let ann = format_error_annotation(&c);
        assert!(ann.contains("[error_type: module_not_found:python:pandas]"));
        assert!(ann.contains("[hint: pip3 install pandas]"));
    }

    #[test]
    fn test_format_error_annotation_no_hint() {
        let c = ErrorClassification {
            kind: ToolErrorKind::FileNotFound {
                path: "/tmp/x".into(),
            },
            hint: None,
        };
        let ann = format_error_annotation(&c);
        assert!(ann.contains("[error_type: file_not_found:/tmp/x]"));
        assert!(!ann.contains("[hint:"));
    }

    #[test]
    fn test_hint_with_uv_available() {
        let env = make_probe(&[("pip3", true), ("uv", true)]);
        let output = "ModuleNotFoundError: No module named 'openpyxl'\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &env);
        assert!(c.hint.as_ref().unwrap().contains("uv pip install"));
    }

    #[test]
    fn test_hint_without_pip() {
        let env = make_probe(&[("python3", true)]);
        let output = "ModuleNotFoundError: No module named 'pandas'\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &env);
        assert!(c.hint.as_ref().unwrap().contains("not available"));
    }

    #[test]
    fn test_extract_submodule() {
        // "No module named 'openpyxl.utils'" should extract "openpyxl"
        let output = "ModuleNotFoundError: No module named 'openpyxl.utils'\n[exit code: 1]";
        let c = classify_error("shell_exec", output, Some(1), &empty_probe());
        assert_eq!(
            c.kind,
            ToolErrorKind::ModuleNotFound {
                module: "openpyxl".into(),
                language: "python".into()
            }
        );
    }
}
