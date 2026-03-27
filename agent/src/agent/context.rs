//! System prompt construction and memory formatting.
//!
//! Loads workspace prompt files (SOUL.md, AGENTS.md, TOOLS.md, USER.md,
//! MEMORY.md) and assembles them into a single system prompt string.

use crate::config::AppConfig;
use crate::memory::Memory;

use super::probe::EnvironmentProbe;
use super::prompt::DEFAULT_SYSTEM_PROMPT;

/// Workspace prompt files, loaded in order.
const WORKSPACE_FILES: &[&str] = &["SOUL.md", "AGENTS.md", "TOOLS.md", "USER.md", "MEMORY.md"];

/// Builds the full system prompt from workspace files or config override.
///
/// Priority:
/// 1. If `config.agent.system_prompt_file` is non-empty → use that file's content.
/// 2. Otherwise, try loading workspace files (SOUL.md → AGENTS.md → TOOLS.md →
///    USER.md → MEMORY.md) from `config.app.workspace`.
/// 3. If none exist → fall back to `DEFAULT_SYSTEM_PROMPT`.
pub async fn build_system_prompt(
    config: &AppConfig,
    skill_summary: &str,
    env_probe: &EnvironmentProbe,
) -> String {
    // Priority 1: explicit system prompt file
    if !config.agent.system_prompt_file.is_empty() {
        if let Ok(content) = tokio::fs::read_to_string(&config.agent.system_prompt_file).await
            && !content.trim().is_empty()
        {
            let mut prompt = content;
            if !skill_summary.is_empty() {
                prompt.push_str("\n\n---\n\n");
                prompt.push_str(skill_summary);
            }
            let env_section = env_probe.to_prompt_section(&config.agent);
            prompt.push_str("\n\n---\n\n");
            prompt.push_str(&env_section);
            return prompt;
        }
        tracing::warn!(
            path = %config.agent.system_prompt_file,
            "configured system_prompt_file not found or empty, falling back"
        );
    }

    // Priority 2: workspace files
    let workspace = &config.app.workspace;
    let mut parts: Vec<String> = Vec::new();

    for filename in WORKSPACE_FILES {
        let path = format!("{}/{}", workspace, filename);
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                parts.push(format!("# {}\n\n{}", filename, trimmed));
            }
        }
        // File not found → skip silently
    }

    let mut prompt = if parts.is_empty() {
        let mut p = DEFAULT_SYSTEM_PROMPT.to_string();
        if !skill_summary.is_empty() {
            p.push_str("\n\n---\n\n");
            p.push_str(skill_summary);
        }
        p
    } else {
        let mut p = parts.join("\n\n---\n\n");
        if !skill_summary.is_empty() {
            p.push_str("\n\n---\n\n");
            p.push_str(skill_summary);
        }
        p
    };

    // Append runtime environment section
    let env_section = env_probe.to_prompt_section(&config.agent);
    prompt.push_str("\n\n---\n\n");
    prompt.push_str(&env_section);
    prompt
}

/// Formats a list of memories into a text block for injection into the system
/// prompt area.
///
/// Returns an empty string if the list is empty.
pub fn format_memories(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }

    let mut out = String::from("# Relevant Memories\n\n");
    for mem in memories {
        out.push_str(&format!("- [{}]: {}\n", mem.key, mem.content));
    }
    out
}
