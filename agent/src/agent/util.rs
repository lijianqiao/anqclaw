//! @file
//! @author <lijianqiao>
//! @since <2026-03-31>
//! @brief 从 AgentCore 提取的纯函数工具集（文件 token 提取、workspace 扫描、描述词提取）。

use std::collections::HashSet;
use std::path::Path;

use super::skill_match::is_description_stopword;

pub(crate) fn extract_file_like_tokens(text: &str) -> HashSet<String> {
    let mut current = String::new();
    let mut tokens = HashSet::new();

    for ch in text.chars() {
        if ch.is_whitespace()
            || matches!(
                ch,
                ',' | '，'
                    | ';'
                    | '；'
                    | ':'
                    | '：'
                    | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
            )
        {
            push_file_token(&mut current, &mut tokens);
            continue;
        }
        current.push(ch);
    }
    push_file_token(&mut current, &mut tokens);

    tokens
}

fn push_file_token(current: &mut String, tokens: &mut HashSet<String>) {
    if current.contains('.') {
        let token = current
            .trim_matches(|ch: char| matches!(ch, '.' | ',' | '，' | ';' | '；' | '"' | '\''))
            .to_lowercase();
        if !token.is_empty() && token.contains('.') {
            tokens.insert(token);
        }
    }
    current.clear();
}

pub(crate) fn collect_workspace_extensions(workspace: &Path, max_files: usize) -> HashSet<String> {
    let mut extensions = HashSet::new();
    if max_files == 0 || !workspace.exists() {
        return extensions;
    }

    let mut visited_files = 0usize;
    let mut stack = vec![workspace.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if visited_files >= max_files {
            break;
        }

        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            if visited_files >= max_files {
                break;
            }

            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            visited_files += 1;
            if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
                let extension = extension.trim().to_lowercase();
                if !extension.is_empty() {
                    extensions.insert(format!(".{extension}"));
                }
            }
        }
    }

    extensions
}

pub(crate) fn extract_description_terms(description: &str) -> HashSet<String> {
    let mut terms = HashSet::new();
    let mut current = String::new();

    for ch in description.chars() {
        if ch.is_alphanumeric() || ch == '.' {
            current.push(ch);
        } else {
            push_description_term(&mut current, &mut terms);
        }
    }
    push_description_term(&mut current, &mut terms);

    terms
}

fn push_description_term(current: &mut String, terms: &mut HashSet<String>) {
    if current.is_empty() {
        return;
    }

    let term = current.trim().to_lowercase();
    current.clear();

    if term.is_empty() {
        return;
    }

    if term.starts_with('.') {
        if term.len() >= 4 {
            terms.insert(term);
        }
        return;
    }

    if term.is_ascii() {
        if term.len() >= 3 && !is_description_stopword(&term) {
            terms.insert(term);
        }
        return;
    }

    let char_count = term.chars().count();
    if char_count >= 2 {
        terms.insert(term.clone());
    }

    if char_count >= 4 {
        let chars: Vec<char> = term.chars().collect();
        for window_size in [3usize, 4usize] {
            if char_count < window_size {
                continue;
            }
            for window in chars.windows(window_size) {
                terms.insert(window.iter().collect());
            }
        }
    }
}
