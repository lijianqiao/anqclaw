//! Skill registry — scans `.md` files with YAML frontmatter from the skills directory.
//!
//! Each skill file has the format:
//! ```markdown
//! ---
//! name: code-review
//! description: Professional code review expert
//! trigger: When the user mentions code review, CR, review
//! ---
//!
//! Full skill prompt content here...
//! ```

use std::path::{Path, PathBuf};

use anyhow::Result;

// ─── SkillMeta ──────────────────────────────────────────────────────────────

/// Metadata extracted from a skill file's YAML frontmatter.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub trigger: String,
    pub path: PathBuf,
}

// ─── SkillRegistry ──────────────────────────────────────────────────────────

/// Holds metadata for all discovered skills. Full content is loaded on demand.
#[derive(Debug)]
pub struct SkillRegistry {
    skills: Vec<SkillMeta>,
}

impl SkillRegistry {
    /// Scan a directory for `.md` files with YAML frontmatter.
    /// Returns a registry with all valid skills found.
    pub fn scan(skills_dir: &Path) -> Self {
        let mut skills = Vec::new();

        if !skills_dir.exists() {
            tracing::debug!(dir = %skills_dir.display(), "skills directory not found, skipping");
            return Self { skills };
        }

        let entries = match std::fs::read_dir(skills_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(dir = %skills_dir.display(), error = %e, "failed to read skills directory");
                return Self { skills };
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                match parse_frontmatter(&path) {
                    Ok(Some(meta)) => {
                        tracing::debug!(name = %meta.name, "loaded skill");
                        skills.push(meta);
                    }
                    Ok(None) => {
                        tracing::debug!(path = %path.display(), "skipping .md file without valid frontmatter");
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "failed to parse skill file");
                    }
                }
            }
        }

        tracing::info!(count = skills.len(), "skills loaded");
        Self { skills }
    }

    /// Returns metadata for all loaded skills.
    pub fn list(&self) -> &[SkillMeta] {
        &self.skills
    }

    /// Find a skill by name.
    pub fn find(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Load the full content of a skill (everything after the frontmatter).
    pub fn load_content(&self, name: &str) -> Result<String> {
        let meta = self.find(name)
            .ok_or_else(|| anyhow::anyhow!("skill `{name}` not found"))?;

        let raw = std::fs::read_to_string(&meta.path)
            .map_err(|e| anyhow::anyhow!("failed to read skill file `{}`: {e}", meta.path.display()))?;

        // Strip frontmatter (everything between the first two `---` lines)
        Ok(strip_frontmatter(&raw))
    }

    /// Build a summary string for injection into the system prompt.
    /// Returns empty string if no skills are loaded.
    pub fn build_summary(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut out = String::from("# Available Skills\n\n");
        out.push_str("You have access to these specialized skills. ");
        out.push_str("When a user's request matches a skill's trigger, call `activate_skill` with the skill name to load it BEFORE responding.\n\n");

        for skill in &self.skills {
            out.push_str(&format!(
                "- **{}**: {}\n  Trigger: {}\n",
                skill.name, skill.description, skill.trigger
            ));
        }

        out.push_str("\nDo NOT guess the skill's content — always call `activate_skill(name)` to load the full prompt.\n");
        out
    }
}

// ─── Frontmatter parsing ────────────────────────────────────────────────────

/// Parse YAML frontmatter from a markdown file.
/// Returns None if no valid frontmatter found.
fn parse_frontmatter(path: &Path) -> Result<Option<SkillMeta>> {
    let content = std::fs::read_to_string(path)?;
    let trimmed = content.trim_start();

    // Must start with `---`
    if !trimmed.starts_with("---") {
        return Ok(None);
    }

    // Find the closing `---`
    let after_opening = &trimmed[3..];
    let close_pos = after_opening.find("\n---");
    let close_pos = match close_pos {
        Some(pos) => pos,
        None => return Ok(None),
    };

    let yaml_block = &after_opening[..close_pos].trim();

    // Parse simple YAML key-value pairs (no need for a full YAML parser)
    let mut name = String::new();
    let mut description = String::new();
    let mut trigger = String::new();

    for line in yaml_block.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => name = value.to_string(),
                "description" => description = value.to_string(),
                "trigger" => trigger = value.to_string(),
                _ => {} // ignore unknown keys
            }
        }
    }

    if name.is_empty() {
        // Use filename as fallback name
        name = path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
    }

    if name.is_empty() {
        return Ok(None);
    }

    // If description is empty, use a generic one
    if description.is_empty() {
        description = format!("Skill loaded from {}", path.file_name().unwrap_or_default().to_string_lossy());
    }

    Ok(Some(SkillMeta {
        name,
        description,
        trigger,
        path: path.to_path_buf(),
    }))
}

/// Strip YAML frontmatter from content, returning only the body.
fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }

    let after_opening = &trimmed[3..];
    if let Some(close_pos) = after_opening.find("\n---") {
        // Skip past the closing `---` and the newline after it
        let body_start = close_pos + 4; // "\n---".len()
        after_opening[body_start..].trim_start().to_string()
    } else {
        content.to_string()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_frontmatter_valid() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_parse");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let skill_path = dir.join("test-skill.md");
        let mut f = std::fs::File::create(&skill_path).unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f, "name: code-review").unwrap();
        writeln!(f, "description: Expert code reviewer").unwrap();
        writeln!(f, "trigger: code review, CR, review").unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "You are a code review expert.").unwrap();

        let meta = parse_frontmatter(&skill_path).unwrap().unwrap();
        assert_eq!(meta.name, "code-review");
        assert_eq!(meta.description, "Expert code reviewer");
        assert_eq!(meta.trigger, "code review, CR, review");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_no_fm");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let skill_path = dir.join("plain.md");
        std::fs::write(&skill_path, "Just a plain markdown file.").unwrap();

        let result = parse_frontmatter(&skill_path).unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_strip_frontmatter() {
        let input = "---\nname: test\ndescription: desc\n---\n\nBody content here.";
        let body = strip_frontmatter(input);
        assert_eq!(body, "Body content here.");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let input = "Just plain text.";
        let body = strip_frontmatter(input);
        assert_eq!(body, "Just plain text.");
    }

    #[test]
    fn test_scan_directory() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_scan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create two skill files
        std::fs::write(
            dir.join("skill-a.md"),
            "---\nname: skill-a\ndescription: Skill A\ntrigger: when A\n---\nContent A",
        ).unwrap();
        std::fs::write(
            dir.join("skill-b.md"),
            "---\nname: skill-b\ndescription: Skill B\ntrigger: when B\n---\nContent B",
        ).unwrap();
        // Create a non-skill file
        std::fs::write(dir.join("readme.txt"), "not a skill").unwrap();

        let registry = SkillRegistry::scan(&dir);
        assert_eq!(registry.list().len(), 2);
        assert!(registry.find("skill-a").is_some());
        assert!(registry.find("skill-b").is_some());
        assert!(registry.find("nonexistent").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_content() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_load");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("my-skill.md"),
            "---\nname: my-skill\ndescription: My Skill\ntrigger: test\n---\n\nFull prompt body here.\nWith multiple lines.",
        ).unwrap();

        let registry = SkillRegistry::scan(&dir);
        let content = registry.load_content("my-skill").unwrap();
        assert!(content.contains("Full prompt body here."));
        assert!(content.contains("With multiple lines."));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_summary() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_summary");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("review.md"),
            "---\nname: code-review\ndescription: Code reviewer\ntrigger: review, CR\n---\nBody",
        ).unwrap();

        let registry = SkillRegistry::scan(&dir);
        let summary = registry.build_summary();
        assert!(summary.contains("code-review"));
        assert!(summary.contains("Code reviewer"));
        assert!(summary.contains("review, CR"));
        assert!(summary.contains("activate_skill"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_empty_skills_dir() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let registry = SkillRegistry::scan(&dir);
        assert!(registry.list().is_empty());
        assert!(registry.build_summary().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nonexistent_skills_dir() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_nonexistent_xyz");
        let _ = std::fs::remove_dir_all(&dir); // make sure it doesn't exist

        let registry = SkillRegistry::scan(&dir);
        assert!(registry.list().is_empty());
    }
}
