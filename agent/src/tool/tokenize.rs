use anyhow::{Result, bail};

pub(crate) fn tokenize_quoted_args(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
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
                        current.push(chars.next().expect("peeked character must exist"));
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
        bail!("unclosed quote in command / 命令中有未闭合的引号")
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.is_empty() {
        bail!("empty command / 空命令")
    }
    Ok(tokens)
}

pub(crate) fn reject_unquoted_shell_metacharacters(input: &str) -> Result<()> {
    let mut chars = input.chars().peekable();
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
                if matches!(chars.peek(), Some('"' | '\\')) {
                    chars.next();
                }
            } else if c == '"' {
                in_double_quote = false;
            }
            continue;
        }

        match c {
            '\'' => in_single_quote = true,
            '"' => in_double_quote = true,
            '|' | '&' | ';' | '<' | '>' | '`' => {
                bail!(
                    "shell metacharacters are not allowed in custom tool commands / 自定义工具命令中不允许 shell 元字符"
                )
            }
            _ => {}
        }
    }

    Ok(())
}
