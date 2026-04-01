//! @file
//! @author lijianqiao
//! @since 2026-03-31
//! @brief 管理 skills 的多源扫描、元数据解析与热重载。

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::agent::util::{
    extract_description_terms, rwlock_read_or_recover, rwlock_write_or_recover,
};

const SKILL_FILE_NAME: &str = "SKILL.md";
const DEFAULT_MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;

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

/// SkillSource：一个 skills 根目录来源。
///
/// 详细说明：source name 仅用于日志和后续覆盖顺序可观测性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    pub name: String,
    pub dir: PathBuf,
}

impl SkillSource {
    /// Create a skill source descriptor.
    ///
    /// # Args
    /// - `name`: 来源名称，例如 `bundled`、`user`、`workspace`。
    /// - `dir`: 来源目录。
    ///
    /// # Returns
    /// - 构造完成的 `SkillSource`。
    pub fn new(name: impl Into<String>, dir: PathBuf) -> Self {
        Self {
            name: name.into(),
            dir,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    trigger: Option<String>,
    keywords: Vec<String>,
    extensions: Vec<String>,
    priority: i32,
    #[serde(alias = "disable-model-invocation")]
    disable_model_invocation: bool,
}

/// Metadata extracted from a skill file's YAML frontmatter.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub trigger: String,
    pub keywords: Vec<String>,
    pub extensions: Vec<String>,
    pub priority: i32,
    pub disable_model_invocation: bool,
    pub source: String,
    pub path: PathBuf,
    normalized_name: String,
    normalized_description: String,
    normalized_description_terms: Vec<String>,
    normalized_triggers: Vec<String>,
    normalized_keywords: Vec<String>,
    normalized_extensions: Vec<String>,
}

impl SkillMeta {
    fn with_normalized_terms(mut self) -> Self {
        self.normalized_name = self.name.trim().to_lowercase();
        self.normalized_description = self.description.trim().to_lowercase();
        self.normalized_description_terms = {
            let mut terms: Vec<String> = extract_description_terms(&self.normalized_description)
                .into_iter()
                .collect();
            terms.sort();
            terms
        };
        self.normalized_triggers = split_terms(&self.trigger);
        self.normalized_keywords = normalize_keywords(&self.keywords);
        self.normalized_extensions = normalize_extensions(&self.extensions);
        self
    }

    /// Return the normalized skill name for matching.
    pub fn normalized_name(&self) -> &str {
        &self.normalized_name
    }

    /// Return the normalized description for matching.
    pub fn normalized_description(&self) -> &str {
        &self.normalized_description
    }

    /// Return normalized description-derived terms for matching.
    pub fn description_terms(&self) -> &[String] {
        &self.normalized_description_terms
    }

    /// Return normalized trigger terms for matching.
    pub fn trigger_terms(&self) -> &[String] {
        &self.normalized_triggers
    }

    /// Return normalized keyword terms for matching.
    pub fn keyword_terms(&self) -> &[String] {
        &self.normalized_keywords
    }

    /// Return normalized extensions with a leading dot.
    pub fn extension_terms(&self) -> &[String] {
        &self.normalized_extensions
    }

    /// Return the normalized file location used in prompt summaries.
    pub fn prompt_location(&self) -> String {
        self.path.to_string_lossy().replace('\\', "/")
    }
}

fn split_terms(value: &str) -> Vec<String> {
    value
        .split([',', '，', '、', ';', '；', '\n'])
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(|term| term.to_lowercase())
        .collect()
}

fn normalize_keywords(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .collect()
}

fn normalize_extensions(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| normalize_extension(value))
        .collect()
}

fn normalize_extension(value: &str) -> Option<String> {
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(if normalized.starts_with('.') {
        normalized
    } else {
        format!(".{normalized}")
    })
}

// ─── SkillRegistry ──────────────────────────────────────────────────────────

/// Holds metadata for all discovered skills. Full content is loaded on demand.
/// Interior mutability via `RwLock` allows hot-reload without replacing the Arc.
#[derive(Debug)]
pub struct SkillRegistry {
    skills: std::sync::RwLock<Vec<SkillMeta>>,
    sources: Vec<SkillSource>,
    max_skill_file_bytes: u64,
}

impl SkillRegistry {
    /// Scan skill sources for packages containing `SKILL.md`.
    /// Returns a registry with all valid skills found.
    pub fn scan(sources: Vec<SkillSource>, max_skill_file_bytes: u64) -> Self {
        let effective_max_bytes = if max_skill_file_bytes == 0 {
            DEFAULT_MAX_SKILL_FILE_BYTES
        } else {
            max_skill_file_bytes
        };
        let skills = scan_skills(&sources, effective_max_bytes);
        tracing::info!(count = skills.len(), "skills loaded / 技能已加载");
        Self {
            skills: std::sync::RwLock::new(skills),
            sources,
            max_skill_file_bytes: effective_max_bytes,
        }
    }

    /// Returns the watched skill sources.
    pub fn sources(&self) -> &[SkillSource] {
        &self.sources
    }

    /// Re-scan all skill sources and atomically replace the skill list.
    pub fn reload(&self) {
        let new_skills = scan_skills(&self.sources, self.max_skill_file_bytes);
        let count = new_skills.len();
        *rwlock_write_or_recover(&self.skills) = new_skills;
        tracing::info!(count, "skills reloaded / 技能已重新加载");
    }

    /// Returns metadata for all loaded skills (cloned snapshot).
    pub fn list(&self) -> Vec<SkillMeta> {
        rwlock_read_or_recover(&self.skills).clone()
    }

    /// Find a skill by name.
    pub fn find(&self, name: &str) -> Option<SkillMeta> {
        rwlock_read_or_recover(&self.skills)
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

    /// Build a structured summary string for injection into the system prompt.
    /// Returns empty string if no prompt-visible skills are available.
    pub fn build_summary(
        &self,
        candidates: &[SkillMeta],
        max_skills_in_prompt: u32,
        max_skill_prompt_chars: u32,
    ) -> String {
        let mut visible: Vec<SkillMeta> = candidates
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .cloned()
            .collect();
        if visible.is_empty() || max_skills_in_prompt == 0 {
            return String::new();
        }

        visible.truncate(max_skills_in_prompt as usize);

        let max_chars = if max_skill_prompt_chars == 0 {
            usize::MAX
        } else {
            max_skill_prompt_chars as usize
        };

        let full = render_available_skills(&visible, false);
        if full.chars().count() <= max_chars {
            return full;
        }

        let mut compact_skills = visible;
        while !compact_skills.is_empty() {
            let compact = render_available_skills(&compact_skills, true);
            if compact.chars().count() <= max_chars {
                return compact;
            }
            compact_skills.pop();
        }

        String::new()
    }
}

fn render_available_skills(skills: &[SkillMeta], compact: bool) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    if compact {
        out.push_str("<available_skills compact=\"true\">\n");
    } else {
        out.push_str("## Skills\n\n");
        out.push_str(
            "Before replying, scan <available_skills> entries. If a skill matches the request, read its SKILL.md before deciding how to proceed.\n\n",
        );
        out.push_str("<available_skills>\n");
    }

    for skill in skills {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
        if !compact {
            out.push_str(&format!(
                "    <description>{}</description>\n",
                escape_xml(&skill.description)
            ));
        }
        out.push_str(&format!(
            "    <location>{}</location>\n",
            escape_xml(&skill.prompt_location())
        ));
        if !compact {
            let extensions = skill.extension_terms();
            if !extensions.is_empty() {
                out.push_str(&format!(
                    "    <extensions>{}</extensions>\n",
                    escape_xml(&extensions.join(","))
                ));
            }
        }
        out.push_str("  </skill>\n");
    }

    out.push_str("</available_skills>");
    out
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ─── Hot-reload watcher ─────────────────────────────────────────────────────

/// Spawns a background file watcher for the skills directory.
/// Returns the watcher handle — caller must keep it alive.
pub fn spawn_skill_watcher(registry: Arc<SkillRegistry>) -> Result<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher, recommended_watcher};

    let sources = registry.sources().to_vec();
    let event_dirs: Vec<PathBuf> = sources.iter().map(|source| source.dir.clone()).collect();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<PathBuf>>(4);

    let mut watcher = recommended_watcher(
        move |res: std::result::Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let dominated = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if dominated {
                    let changed_paths: Vec<PathBuf> = event
                        .paths
                        .into_iter()
                        .filter(|path| {
                            event_dirs
                                .iter()
                                .any(|dir| is_relevant_skill_event_path(dir, path))
                        })
                        .collect();
                    if !changed_paths.is_empty() {
                        let _ = tx.try_send(changed_paths);
                    }
                }
            }
        },
    )?;

    for source in &sources {
        if source.dir.exists() {
            watcher.watch(&source.dir, RecursiveMode::Recursive)?;
        } else {
            tracing::debug!(
                source = source.name.as_str(),
                dir = %source.dir.display(),
                "skill source directory not found, watcher skipped / 技能来源目录不存在，已跳过监视"
            );
        }
    }

    tokio::spawn(async move {
        let debounce = tokio::time::Duration::from_secs(1);
        while let Some(first_paths) = rx.recv().await {
            let mut changed_paths = BTreeSet::new();
            changed_paths.extend(first_paths);
            // Debounce: wait, then drain any queued events
            tokio::time::sleep(debounce).await;
            while let Ok(paths) = rx.try_recv() {
                changed_paths.extend(paths);
            }
            let changed_paths: Vec<String> = changed_paths
                .into_iter()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .collect();
            tracing::info!(
                count = changed_paths.len(),
                paths = ?changed_paths,
                "skills reload triggered by file changes / 技能热重载由文件变化触发"
            );
            registry.reload();
        }
    });

    Ok(watcher)
}

// ─── Frontmatter parsing ────────────────────────────────────────────────────

/// Scan a directory for skill packages containing `SKILL.md`, returning skill metadata.
fn scan_skills(sources: &[SkillSource], max_skill_file_bytes: u64) -> Vec<SkillMeta> {
    let mut skills: BTreeMap<String, SkillMeta> = BTreeMap::new();

    for source in sources {
        for meta in scan_source_skills(source, max_skill_file_bytes) {
            let skill_name = meta.name.clone();
            match skills.entry(skill_name.clone()) {
                Entry::Occupied(mut entry) => {
                    let previous = entry.get();
                    tracing::warn!(
                        skill = skill_name.as_str(),
                        previous_source = previous.source.as_str(),
                        new_source = meta.source.as_str(),
                        previous_path = %previous.path.display(),
                        new_path = %meta.path.display(),
                        "skill overridden by later source / 技能被后续来源覆盖"
                    );
                    entry.insert(meta);
                }
                Entry::Vacant(entry) => {
                    entry.insert(meta);
                }
            }
        }
    }

    skills.into_values().collect()
}

fn scan_source_skills(source: &SkillSource, max_skill_file_bytes: u64) -> Vec<SkillMeta> {
    let mut skills = Vec::new();

    if !source.dir.exists() {
        tracing::debug!(
            source = source.name.as_str(),
            dir = %source.dir.display(),
            "skill source directory not found, skipping / 技能来源目录未找到，已跳过"
        );
        return skills;
    }

    let entries = match std::fs::read_dir(&source.dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                source = source.name.as_str(),
                dir = %source.dir.display(),
                error = %error,
                "failed to read skill source directory / 读取技能来源目录失败"
            );
            return skills;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            if is_legacy_skill_file(&path) {
                tracing::warn!(
                    source = source.name.as_str(),
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

        match parse_frontmatter(&skill_path, source, max_skill_file_bytes) {
            Ok(Some(meta)) => {
                tracing::debug!(name = %meta.name, source = source.name.as_str(), "loaded skill / 已加载技能");
                skills.push(meta);
            }
            Ok(None) => {
                tracing::debug!(path = %skill_path.display(), "skill package skipped after validation / 技能包校验后已跳过");
            }
            Err(error) => {
                tracing::warn!(path = %skill_path.display(), error = %error, "failed to parse skill file / 解析技能文件失败");
            }
        }
    }

    skills
}

/// Parse YAML frontmatter from a markdown file.
/// Returns None if no valid frontmatter found.
fn parse_frontmatter(
    path: &Path,
    source: &SkillSource,
    max_skill_file_bytes: u64,
) -> Result<Option<SkillMeta>> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("failed to stat skill file `{}`", path.display()))?;
    if metadata.len() > max_skill_file_bytes {
        tracing::warn!(
            source = source.name.as_str(),
            path = %path.display(),
            size_bytes = metadata.len(),
            max_bytes = max_skill_file_bytes,
            "skill file exceeds max size and was skipped / 技能文件超过大小上限，已跳过"
        );
        return Ok(None);
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read skill file `{}`", path.display()))?;
    let Some(yaml_block) = extract_frontmatter(&content) else {
        tracing::warn!(
            source = source.name.as_str(),
            path = %path.display(),
            "skill file missing YAML frontmatter / 技能文件缺少 YAML 前置元数据"
        );
        return Ok(None);
    };

    let frontmatter: SkillFrontmatter = match serde_yaml::from_str(yaml_block) {
        Ok(frontmatter) => frontmatter,
        Err(error) => {
            tracing::warn!(
                source = source.name.as_str(),
                path = %path.display(),
                error = %error,
                "invalid skill frontmatter / 技能前置元数据非法"
            );
            return Ok(None);
        }
    };

    let fallback_name = fallback_skill_name(path).trim().to_string();
    let name = match frontmatter.name.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ if !fallback_name.is_empty() => {
            tracing::warn!(
                source = source.name.as_str(),
                path = %path.display(),
                fallback = fallback_name.as_str(),
                "skill name missing, using directory fallback / 技能名称缺失，使用目录名回退"
            );
            fallback_name
        }
        _ => {
            tracing::warn!(
                source = source.name.as_str(),
                path = %path.display(),
                "skill name missing and no fallback available / 技能名称缺失且无可用回退，已跳过"
            );
            return Ok(None);
        }
    };
    if name.is_empty() {
        return Ok(None);
    }

    let description = match frontmatter.description.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => {
            let fallback_description = format!(
                "Skill loaded from {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            tracing::warn!(
                source = source.name.as_str(),
                path = %path.display(),
                description = fallback_description.as_str(),
                "skill description missing, using default / 技能描述缺失，使用默认值"
            );
            fallback_description
        }
    };

    let trigger = frontmatter.trigger.unwrap_or_default().trim().to_string();

    Ok(Some(
        SkillMeta {
            name,
            description,
            trigger,
            keywords: normalize_list(frontmatter.keywords),
            extensions: normalize_list(frontmatter.extensions),
            priority: frontmatter.priority,
            disable_model_invocation: frontmatter.disable_model_invocation,
            source: source.name.clone(),
            path: path.to_path_buf(),
            normalized_name: String::new(),
            normalized_description: String::new(),
            normalized_description_terms: vec![],
            normalized_triggers: vec![],
            normalized_keywords: vec![],
            normalized_extensions: vec![],
        }
        .with_normalized_terms(),
    ))
}

fn extract_frontmatter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_opening = &trimmed[3..];
    let close_pos = after_opening.find("\n---")?;
    Some(after_opening[..close_pos].trim())
}

fn fallback_skill_name(path: &Path) -> String {
    if path.file_name().is_some_and(|file| file == SKILL_FILE_NAME) {
        path.parent()
            .and_then(|parent| parent.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

fn normalize_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
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

    const TEST_MAX_SKILL_FILE_BYTES: u64 = 16 * 1024;

    fn source(name: &str, dir: &Path) -> SkillSource {
        SkillSource::new(name, dir.to_path_buf())
    }

    fn scan_single_source(dir: &Path) -> SkillRegistry {
        SkillRegistry::scan(vec![source("workspace", dir)], TEST_MAX_SKILL_FILE_BYTES)
    }

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
        writeln!(f, "keywords:").unwrap();
        writeln!(f, "  - reviewer").unwrap();
        writeln!(f, "  - static-analysis").unwrap();
        writeln!(f, "extensions:").unwrap();
        writeln!(f, "  - .rs").unwrap();
        writeln!(f, "  - .py").unwrap();
        writeln!(f, "priority: 80").unwrap();
        writeln!(f, "disable_model_invocation: true").unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "You are a code review expert.").unwrap();

        let meta = parse_frontmatter(
            &skill_path,
            &source("workspace", &dir),
            TEST_MAX_SKILL_FILE_BYTES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(meta.name, "code-review");
        assert_eq!(meta.description, "Expert code reviewer");
        assert_eq!(meta.trigger, "code review, CR, review");
        assert_eq!(meta.keywords, vec!["reviewer", "static-analysis"]);
        assert_eq!(meta.extensions, vec![".rs", ".py"]);
        assert_eq!(meta.normalized_name(), "code-review");
        assert_eq!(meta.normalized_description(), "expert code reviewer");
        assert_eq!(meta.description_terms(), &["code", "expert", "reviewer"]);
        assert_eq!(meta.trigger_terms(), &["code review", "cr", "review"]);
        assert_eq!(meta.keyword_terms(), &["reviewer", "static-analysis"]);
        assert_eq!(meta.extension_terms(), &[".rs", ".py"]);
        assert_eq!(meta.priority, 80);
        assert!(meta.disable_model_invocation);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_no_fm");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let skill_path = dir.join(SKILL_FILE_NAME);
        std::fs::write(&skill_path, "Just a plain markdown file.").unwrap();

        let result = parse_frontmatter(
            &skill_path,
            &source("workspace", &dir),
            TEST_MAX_SKILL_FILE_BYTES,
        )
        .unwrap();
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

        let registry = scan_single_source(&dir);
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

        let registry = scan_single_source(&dir);
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
            "---\nname: code-review\ndescription: Code reviewer\ntrigger: review, CR\nextensions:\n  - .rs\n  - .py\n---\nBody",
        );

        let registry = scan_single_source(&dir);
        let summary = registry.build_summary(&registry.list(), 8, 4_000);
        assert!(summary.contains("code-review"));
        assert!(summary.contains("Code reviewer"));
        assert!(summary.contains("<available_skills>"));
        assert!(summary.contains("<location>"));
        assert!(summary.contains("<extensions>"));
        assert!(!summary.contains("activate_skill"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_empty_skills_dir() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let registry = scan_single_source(&dir);
        assert!(registry.list().is_empty());
        assert!(
            registry
                .build_summary(&registry.list(), 8, 4_000)
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nonexistent_skills_dir() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_nonexistent_xyz");
        let _ = std::fs::remove_dir_all(&dir); // make sure it doesn't exist

        let registry = scan_single_source(&dir);
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

        let meta = parse_frontmatter(
            &skill_path,
            &source("workspace", &dir),
            TEST_MAX_SKILL_FILE_BYTES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(meta.name, "xlsx");
        assert!(!meta.disable_model_invocation);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_frontmatter_defaults_description_when_missing() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_default_description");
        let _ = std::fs::remove_dir_all(&dir);
        let skill_path = create_skill_package(
            &dir,
            "xlsx",
            "---\nname: xlsx\ntrigger: xlsx, csv\n---\nBody",
        );

        let meta = parse_frontmatter(
            &skill_path,
            &source("workspace", &dir),
            TEST_MAX_SKILL_FILE_BYTES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(meta.description, "Skill loaded from SKILL.md");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_multi_source_override_prefers_later_source() {
        let root = std::env::temp_dir().join("anqclaw_test_skills_override_sources");
        let user_dir = root.join("user");
        let workspace_dir = root.join("workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::create_dir_all(&workspace_dir).unwrap();

        create_skill_package(
            &user_dir,
            "xlsx",
            "---\nname: xlsx\ndescription: User skill\ntrigger: xlsx\n---\nUser",
        );
        create_skill_package(
            &workspace_dir,
            "xlsx",
            "---\nname: xlsx\ndescription: Workspace skill\ntrigger: xlsx\n---\nWorkspace",
        );

        let registry = SkillRegistry::scan(
            vec![
                source("user", &user_dir),
                source("workspace", &workspace_dir),
            ],
            TEST_MAX_SKILL_FILE_BYTES,
        );
        let skill = registry.find("xlsx").unwrap();
        assert_eq!(skill.description, "Workspace skill");
        assert_eq!(skill.source, "workspace");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_scan_skips_invalid_yaml_frontmatter() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_invalid_yaml");
        let _ = std::fs::remove_dir_all(&dir);
        create_skill_package(&dir, "broken", "---\nname: [broken\n---\nBody");

        let registry = scan_single_source(&dir);
        assert!(registry.find("broken").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_skips_skill_file_over_limit() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_oversized");
        let _ = std::fs::remove_dir_all(&dir);
        let body = format!(
            "---\nname: giant\ndescription: Large skill\n---\n{}",
            "x".repeat(2_048)
        );
        create_skill_package(&dir, "giant", &body);

        let registry = SkillRegistry::scan(vec![source("workspace", &dir)], 128);
        assert!(registry.find("giant").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_summary_compacts_when_prompt_budget_is_small() {
        let dir = PathBuf::from("compact_skill_test_tmp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        create_skill_package(
            &dir,
            "xlsx",
            "---\nname: xlsx\ndescription: Spreadsheet helper with a very long description that should force the prompt renderer to switch into compact mode once the prompt budget becomes tight\nextensions:\n  - .xlsx\n  - .csv\n---\nBody",
        );

        let registry = scan_single_source(&dir);
        let summary = registry.build_summary(&registry.list(), 8, 230);
        assert!(summary.contains("<available_skills compact=\"true\">"));
        assert!(summary.contains("<name>xlsx</name>"));
        assert!(summary.contains("<location>"));
        assert!(!summary.contains("<description>"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_summary_hides_disable_model_invocation_skills() {
        let dir = std::env::temp_dir().join("anqclaw_test_skills_summary_hidden");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        create_skill_package(
            &dir,
            "hidden-skill",
            "---\nname: hidden-skill\ndescription: Hidden\ndisable_model_invocation: true\n---\nBody",
        );
        create_skill_package(
            &dir,
            "visible-skill",
            "---\nname: visible-skill\ndescription: Visible\n---\nBody",
        );

        let registry = scan_single_source(&dir);
        let summary = registry.build_summary(&registry.list(), 8, 4_000);
        assert!(summary.contains("visible-skill"));
        assert!(!summary.contains("hidden-skill"));

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
