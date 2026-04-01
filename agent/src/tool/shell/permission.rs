//! @file
//! @author <lijianqiao>
//! @since <2026-03-31>
//! @brief 负责 shell_exec 的权限级别与命令链切分规则。

#[cfg(target_os = "windows")]
const WINDOWS_SHELL_BUILTINS: &[&str] = &["echo", "dir", "type", "cd", "set"];

/// Built-in readonly commands — safe to run in any mode.
pub(crate) const READONLY_COMMANDS: &[&str] = &[
    "ls", "dir", "cat", "head", "tail", "grep", "date", "whoami", "pwd", "wc", "sort", "uniq",
    "echo", "file", "stat", "type", "where", "hostname", "uname", "df", "du", "printenv", "which",
];

/// Commands that are ALWAYS blocked regardless of permission level.
pub(crate) const ALWAYS_BLOCKED: &[&str] = &[
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

pub(crate) fn is_shell_builtin(command: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        WINDOWS_SHELL_BUILTINS.contains(&command)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = command;
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionLevel {
    Readonly,
    Supervised,
    Full,
}

impl PermissionLevel {
    pub fn parse(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "readonly" => Self::Readonly,
            "full" => Self::Full,
            _ => Self::Supervised,
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

    while let Some(ch) = chars.next() {
        if in_single_quote {
            current.push(ch);
            if ch == '\'' {
                in_single_quote = false;
            }
            continue;
        }
        if in_double_quote {
            if ch == '\\' {
                current.push(ch);
                if let Some(&next) = chars.peek()
                    && (next == '"' || next == '\\')
                {
                    current.push(chars.next().expect("peeked character must exist"));
                }
            } else if ch == '"' {
                current.push(ch);
                in_double_quote = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' => {
                current.push(ch);
                in_single_quote = true;
            }
            '"' => {
                current.push(ch);
                in_double_quote = true;
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    segments.push(trimmed);
                }
                current.clear();
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        segments.push(trimmed);
                    }
                    current.clear();
                } else {
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
                        current.push(ch);
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
                    current.push(ch);
                }
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        segments.push(trimmed);
    }

    segments
}
