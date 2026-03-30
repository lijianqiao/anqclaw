//! Skill registry — scans directory-based skill packages from the skills directory.
//!
//! Each skill package has the format:
//! ```markdown
//! skills/<skill-name>/SKILL.md
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

const SKILL_FILE_NAME: &str = "SKILL.md";

fn is_legacy_skill_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name != SKILL_FILE_NAME)
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

fn is_relevant_skill_event_path(skills_dir: &Path, path: &Path) -> bool {
    if path.file_name().is_some_and(|name| name == SKILL_FILE_NAME) {
        return path
            .parent()
            .and_then(|parent| parent.parent())
            .is_some_and(|grandparent| grandparent == skills_dir);
    }

    path.parent().is_some_and(|parent| parent == skills_dir)
}

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
/// Interior mutability via `RwLock` allows hot-reload without replacing the Arc.
#[derive(Debug)]
pub struct SkillRegistry {
    skills: std::sync::RwLock<Vec<SkillMeta>>,
    dir: PathBuf,
}

impl SkillRegistry {
    /// Scan a directory for skill packages containing `SKILL.md`.
    /// Returns a registry with all valid skills found.
    pub fn scan(skills_dir: &Path) -> Self {
        let skills = scan_skills(skills_dir);
        tracing::info!(count = skills.len(), "skills loaded / 技能已加载");
        Self {
            skills: std::sync::RwLock::new(skills),
            dir: skills_dir.to_path_buf(),
        }
    }

    /// Returns the watched directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Re-scan the skills directory and atomically replace the skill list.
    pub fn reload(&self) {
        let new_skills = scan_skills(&self.dir);
        let count = new_skills.len();
        // unwrap_or_else recovers from poisoned lock (previous panic while holding write)
        match self.skills.write() {
            Ok(mut guard) => *guard = new_skills,
            Err(e) => *e.into_inner() = new_skills,
        }
        tracing::info!(count, "skills reloaded / 技能已重新加载");
    }

    /// Returns metadata for all loaded skills (cloned snapshot).
    pub fn list(&self) -> Vec<SkillMeta> {
        self.skills
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Find a skill by name.
    pub fn find(&self, name: &str) -> Option<SkillMeta> {
        self.skills
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|s| s.name == name)
            .cloned()
    }

    /// Load the full content of a skill (everything after the frontmatter).
    pub fn load_content(&self, name: &str) -> Result<String> {
        let meta = self
            .find(name)
            .ok_or_else(|| anyhow::anyhow!("skill `{name}` not found / 技能 `{name}` 未找到"))?;

        let raw = std::fs::read_to_string(&meta.path).map_err(|e| {
            anyhow::anyhow!(
                "failed to read skill file `{}`: {e} / 读取技能文件失败",
                meta.path.display()
            )
        })?;

        Ok(strip_frontmatter(&raw))
    }

    /// Build a summary string for injection into the system prompt.
    /// Returns empty string if no skills are loaded.
    pub fn build_summary(&self) -> String {
        let skills = self.skills.read().unwrap_or_else(|e| e.into_inner());
        if skills.is_empty() {
            return String::new();
        }

        let mut out = String::from("# Available Skills\n\n");
        out.push_str("You have access to these specialized skills. ");
        out.push_str("When a user's request matches a skill's trigger, call `activate_skill` with the skill name to load it BEFORE responding.\n\n");

        for skill in skills.iter() {
            out.push_str(&format!("- **{}**: {}\n", skill.name, skill.description));
            if !skill.trigger.trim().is_empty() {
                out.push_str(&format!("  Trigger: {}\n", skill.trigger));
            }
        }

        out.push_str(
            "\nDo NOT guess the skill's content — always call `activate_skill(name)` to load the full prompt.\n",
        );
        out
    }
}

// ─── Hot-reload watcher ─────────────────────────────────────────────────────

/// Spawns a background file watcher for the skills directory.
/// Returns the watcher handle — caller must keep it alive.
pub fn spawn_skill_watcher(registry: Arc<SkillRegistry>) -> Result<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher, recommended_watcher};

    let dir = registry.dir().to_path_buf();
    let event_dir = dir.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(4);

    let mut watcher = recommended_watcher(
        move |res: std::result::Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let dominated = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if dominated
                    && event
                        .paths
                        .iter()
                        .any(|path| is_relevant_skill_event_path(&event_dir, path))
                {
                    let _ = tx.try_send(());
                }
            }
        },
    )?;

    watcher.watch(&dir, RecursiveMode::Recursive)?;

    tokio::spawn(async move {
        let debounce = tokio::time::Duration::from_secs(1);
        while rx.recv().await.is_some() {
            // Debounce: wait, then drain any queued events
            tokio::time::sleep(debounce).await;
            while rx.try_recv().is_ok() {}
            registry.reload();
        }
    });

    Ok(watcher)
}

// ─── Frontmatter parsing ────────────────────────────────────────────────────

/// Scan a directory for skill packages containing `SKILL.md`, returning skill metadata.
fn scan_skills(skills_dir: &Path) -> Vec<SkillMeta> {
    let mut skills = Vec::new();

    if !skills_dir.exists() {
        tracing::debug!(dir = %skills_dir.display(), "skills directory not found, skipping / 技能目录未找到，已跳过");
        return skills;
    }

    let entries = match std::fs::read_dir(skills_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %skills_dir.display(), error = %e, "failed to read skills directory / 读取技能目录失败");
            return skills;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            if is_legacy_skill_file(&path) {
                tracing::warn!(
                    path = %path.display(),
                    "legacy flat skill file ignored; expected skills/<name>/SKILL.md / 已忽略遗留平铺技能文件；当前仅支持 skills/<name>/SKILL.md"
                );
            } else {
                tracing::debug!(path = %path.display(), "skipping non-directory skill entry / 跳过非目录技能项");
            }
            continue;
        }

        let skill_path = path.join(SKILL_FILE_NAME);
        if !skill_path.is_file() {
            tracing::debug!(path = %path.display(), "skipping skill directory without SKILL.md / 跳过缺少 SKILL.md 的技能目录");
            continue;
        }

        match parse_frontmatter(&skill_path) {
            Ok(Some(meta)) => {
                tracing::debug!(name = %meta.name, "loaded skill / 已加载技能");
                skills.push(meta);
            }
            Ok(None) => {
                tracing::debug!(path = %skill_path.display(), "skipping SKILL.md without valid frontmatter / 跳过无有效前置元数据的 SKILL.md");
            }
            Err(e) => {
                tracing::warn!(path = %skill_path.display(), error = %e, "failed to parse skill file / 解析技能文件失败");
            }
        }
    }

    skills
}

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
        name = if path.file_name().is_some_and(|file| file == SKILL_FILE_NAME) {
            path.parent()
                .and_then(|parent| parent.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };
    }

    if name.is_empty() {
        return Ok(None);
    }

    // If description is empty, use a generic one
    if description.is_empty() {
        description = format!(
            "Skill loaded from {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
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

    fn create_skill_package(root: &Path, name: &str, body: &str) -> PathBuf {
        let skill_dir = root.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_path = skill_dir.join(SKILL_FILE_NAME);
        std::fs::write(&skill_path, body).unwrap();
        skill_path
    }

    #[test]
    fn test_parse_frontmatter_valid() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_parse");
        let _ = std::fs::remove_dir_all(&dir);
        let skill_path = create_skill_package(&dir, "code-review", "");
        let mut f = std::fs::File::create(&skill_path).unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f, "name: code-review").unwrap();
        writeln!(f, "description: Expert code reviewer").unwrap();
        writeln!(f, "trigger: code review, CR, review").unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f).unwrap();
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

        let skill_path = dir.join(SKILL_FILE_NAME);
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

        create_skill_package(
            &dir,
            "skill-a",
            "---\nname: skill-a\ndescription: Skill A\ntrigger: when A\n---\nContent A",
        );
        create_skill_package(
            &dir,
            "skill-b",
            "---\nname: skill-b\ndescription: Skill B\ntrigger: when B\n---\nContent B",
        );
        std::fs::write(
            dir.join("legacy.md"),
            "---\nname: legacy\ndescription: Legacy Skill\ntrigger: legacy\n---\nLegacy",
        )
        .unwrap();

        let registry = SkillRegistry::scan(&dir);
        assert_eq!(registry.list().len(), 2);
        assert!(registry.find("skill-a").is_some());
        assert!(registry.find("skill-b").is_some());
        assert!(registry.find("legacy").is_none());
        assert!(registry.find("nonexistent").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_content() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_load");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        create_skill_package(
            &dir,
            "my-skill",
            "---\nname: my-skill\ndescription: My Skill\ntrigger: test\n---\n\nFull prompt body here.\nWith multiple lines.",
        );

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

        create_skill_package(
            &dir,
            "code-review",
            "---\nname: code-review\ndescription: Code reviewer\ntrigger: review, CR\n---\nBody",
        );

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

    #[test]
    fn test_parse_frontmatter_uses_directory_name_for_skill_md_fallback() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_fallback_name");
        let _ = std::fs::remove_dir_all(&dir);
        let skill_path = create_skill_package(
            &dir,
            "xlsx",
            "---\ndescription: Spreadsheet helper\ntrigger: xlsx, csv\n---\nBody",
        );

        let meta = parse_frontmatter(&skill_path).unwrap().unwrap();
        assert_eq!(meta.name, "xlsx");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_relevant_skill_event_path_filters_nested_noise() {
        let root = PathBuf::from("C:/skills");

        assert!(is_relevant_skill_event_path(
            &root,
            &root.join("xlsx").join(SKILL_FILE_NAME)
        ));
        assert!(is_relevant_skill_event_path(&root, &root.join("xlsx")));
        assert!(!is_relevant_skill_event_path(
            &root,
            &root.join("xlsx").join("scripts").join("analyze.py")
        ));
        assert!(!is_relevant_skill_event_path(
            &root,
            &root.join("xlsx").join("README.md")
        ));
    }

    #[test]
    fn test_detects_legacy_flat_skill_file() {
        assert!(is_legacy_skill_file(Path::new("legacy.md")));
        assert!(is_legacy_skill_file(Path::new("LEGACY.MD")));
        assert!(!is_legacy_skill_file(Path::new(SKILL_FILE_NAME)));
        assert!(!is_legacy_skill_file(Path::new("legacy.txt")));
    }
}
