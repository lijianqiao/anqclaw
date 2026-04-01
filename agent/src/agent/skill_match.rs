//! @file
//! @author <lijianqiao>
//! @since <2026-03-31>
//! @brief Skill 候选匹配与评分逻辑，从 AgentCore 中提取以降低模块复杂度。

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use crate::skill::SkillMeta;
use crate::types::ChatMessage;

use super::util::{
    collect_workspace_extensions, extract_file_like_tokens, rwlock_read_or_recover,
    rwlock_write_or_recover,
};

const RECENT_HISTORY_MESSAGE_LIMIT: usize = 8;

const DESCRIPTION_STOPWORDS: &[&str] = &[
    "the",
    "and",
    "when",
    "with",
    "this",
    "that",
    "from",
    "into",
    "your",
    "their",
    "then",
    "than",
    "use",
    "using",
    "used",
    "skill",
    "time",
    "any",
    "want",
    "wants",
    "user",
    "users",
    "file",
    "files",
    "primary",
    "input",
    "output",
    "trigger",
    "especially",
    "existing",
    "these",
    "those",
];

#[derive(Default)]
pub(super) struct SkillCandidateContext {
    pub user_text: String,
    pub history_text: String,
    pub combined_text: String,
    pub recent_file_tokens: HashSet<String>,
    pub workspace_extensions: HashSet<String>,
}

#[derive(Clone)]
pub(super) struct SkillCandidateMatch {
    pub skill: SkillMeta,
    pub score: i32,
    pub description_match: bool,
    pub extension_match: bool,
}

#[derive(Clone, Copy, Default)]
struct MatchSignalScore {
    score: i32,
    matched: bool,
}

impl super::AgentCore {
    #[cfg(test)]
    pub(super) async fn select_skill_candidates(
        &self,
        user_text: &str,
        history: &[ChatMessage],
    ) -> Vec<SkillMeta> {
        self.select_skill_candidate_matches(user_text, history)
            .await
            .into_iter()
            .map(|candidate| candidate.skill)
            .collect()
    }

    pub(super) async fn select_skill_candidate_matches(
        &self,
        user_text: &str,
        history: &[ChatMessage],
    ) -> Vec<SkillCandidateMatch> {
        let Some(registry) = self.skill_registry.as_ref() else {
            return Vec::new();
        };

        let max_active = self.config.skills.max_active_skills as usize;
        if max_active == 0 {
            return Vec::new();
        }

        let context = self.build_skill_candidate_context(user_text, history).await;
        let mut scored: Vec<SkillCandidateMatch> = registry
            .list()
            .into_iter()
            .filter_map(|skill| score_skill_candidate(&skill, &context))
            .collect();

        scored.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.skill.priority.cmp(&left.skill.priority))
                .then_with(|| left.skill.name.cmp(&right.skill.name))
        });

        scored.into_iter().take(max_active).collect()
    }

    pub(super) async fn build_skill_candidate_context(
        &self,
        user_text: &str,
        history: &[ChatMessage],
    ) -> SkillCandidateContext {
        let user_text = user_text.to_lowercase();
        let mut history_text = String::new();
        for message in history
            .iter()
            .rev()
            .filter(|message| message.role != crate::types::Role::System)
            .take(RECENT_HISTORY_MESSAGE_LIMIT)
        {
            if !history_text.is_empty() {
                history_text.push('\n');
            }
            history_text.push_str(&message.content.to_lowercase());
        }
        let combined_text = if history_text.is_empty() {
            user_text.clone()
        } else {
            format!("{user_text}\n{history_text}")
        };

        SkillCandidateContext {
            recent_file_tokens: extract_file_like_tokens(&combined_text),
            workspace_extensions: self.cached_workspace_extensions().await,
            user_text,
            history_text,
            combined_text,
        }
    }

    pub(super) async fn cached_workspace_extensions(&self) -> HashSet<String> {
        {
            let cache_guard = rwlock_read_or_recover(&self.workspace_extensions_cache);
            if let Some(cache) = cache_guard.as_ref()
                && cache.cached_at.elapsed() < super::WORKSPACE_EXTENSIONS_CACHE_TTL
            {
                return cache.extensions.clone();
            }
        }

        let workspace = self.config.app.workspace.clone();
        let extensions = if tokio::runtime::Handle::try_current().is_ok() {
            let workspace_for_scan = workspace.clone();
            tokio::task::spawn_blocking(move || {
                collect_workspace_extensions(
                    Path::new(&workspace_for_scan),
                    super::WORKSPACE_EXTENSION_SCAN_LIMIT,
                )
            })
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "workspace extension scan task failed, falling back to inline scan / 工作区扩展扫描后台任务失败，回退为同步扫描"
                );
                collect_workspace_extensions(
                    Path::new(&workspace),
                    super::WORKSPACE_EXTENSION_SCAN_LIMIT,
                )
            })
        } else {
            collect_workspace_extensions(
                Path::new(&workspace),
                super::WORKSPACE_EXTENSION_SCAN_LIMIT,
            )
        };
        let refreshed_cache = super::WorkspaceExtensionsCache {
            cached_at: Instant::now(),
            extensions: extensions.clone(),
        };

        *rwlock_write_or_recover(&self.workspace_extensions_cache) = Some(refreshed_cache);

        extensions
    }
}

fn score_skill_candidate(
    skill: &SkillMeta,
    context: &SkillCandidateContext,
) -> Option<SkillCandidateMatch> {
    if skill.disable_model_invocation {
        return None;
    }

    let mut score = skill.priority;
    let mut matched = false;
    let skill_name = skill.normalized_name();
    let mut description_match = false;
    let mut extension_match = false;

    if !skill_name.is_empty() {
        if context.user_text.contains(skill_name) {
            score += 120;
            matched = true;
        }
        if context.history_text.contains(skill_name) {
            score += 60;
            matched = true;
        }
    }

    let description_signal = score_description_match(skill, context);
    if description_signal.matched {
        score += description_signal.score;
        matched = true;
        description_match = true;
    }

    for keyword in skill.keyword_terms() {
        if context.user_text.contains(keyword.as_str()) {
            score += 80;
            matched = true;
        }
        if context.history_text.contains(keyword.as_str()) {
            score += 40;
            matched = true;
        }
    }

    for trigger in skill.trigger_terms() {
        if context.user_text.contains(trigger.as_str()) {
            score += 40;
            matched = true;
        }
        if context.history_text.contains(trigger.as_str()) {
            score += 20;
            matched = true;
        }
    }

    let extension_signal = score_extension_match(skill, context);
    if extension_signal.matched {
        score += extension_signal.score;
        matched = true;
        extension_match = true;
    }

    matched.then_some(SkillCandidateMatch {
        skill: skill.clone(),
        score,
        description_match,
        extension_match,
    })
}

fn score_description_match(skill: &SkillMeta, context: &SkillCandidateContext) -> MatchSignalScore {
    let description = skill.normalized_description();
    if description.is_empty() {
        return MatchSignalScore::default();
    }

    let mut score = 0;
    if context.user_text.contains(description) {
        score += 50;
    } else {
        let terms = skill.description_terms();
        if terms.iter().any(|term| context.user_text.contains(term)) {
            score += 50;
        }
        if terms.iter().any(|term| context.history_text.contains(term)) {
            score += 25;
        }
        return MatchSignalScore {
            score,
            matched: score > 0,
        };
    }

    if context.history_text.contains(description) {
        score += 25;
    }
    MatchSignalScore {
        score,
        matched: score > 0,
    }
}

fn score_extension_match(skill: &SkillMeta, context: &SkillCandidateContext) -> MatchSignalScore {
    const EXTENSION_CONTENT_MATCH_SCORE: i32 = 90;
    const EXTENSION_WORKSPACE_MATCH_SCORE: i32 = 15;

    let mut score = 0;
    let mut content_extension_matched = false;
    let mut workspace_extension_matched = false;

    for extension in skill.extension_terms() {
        if !content_extension_matched
            && (context.combined_text.contains(extension.as_str())
                || context
                    .recent_file_tokens
                    .iter()
                    .any(|token| token.ends_with(extension.as_str())))
        {
            score += EXTENSION_CONTENT_MATCH_SCORE;
            content_extension_matched = true;
        }

        if context.workspace_extensions.contains(extension.as_str()) {
            workspace_extension_matched = true;
        }
    }

    if workspace_extension_matched {
        score += EXTENSION_WORKSPACE_MATCH_SCORE;
    }

    MatchSignalScore {
        score,
        matched: score > 0,
    }
}

/// Exported for unit tests: re-export DESCRIPTION_STOPWORDS so tests in mod.rs
/// can access it.
pub(super) fn is_description_stopword(word: &str) -> bool {
    DESCRIPTION_STOPWORDS.contains(&word)
}
