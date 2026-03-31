//! Agent Core — agentic loop with LLM ↔ tool calling.
//!
//! TODO(future): When splitting into workspace crates, extract into
//! `crates/agent/` with its own `Cargo.toml`.

pub mod context;
pub mod probe;
pub mod prompt;
pub mod redact;
mod token_budget;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audit::AuditLogger;
use crate::config::AppConfig;
use crate::llm::{LlmClient, create_llm_client};
use crate::memory::MemoryStore;
use crate::skill::{SkillMeta, SkillRegistry};
use crate::tool::ToolRegistry;
use crate::types::{ChatMessage, InboundMessage, OutboundMessage, StreamEvent};

use context::{build_system_prompt, format_memories};
use probe::EnvironmentProbe;
use token_budget::trim_messages_to_budget;

// ─── AgentCore ───────────────────────────────────────────────────────────────

pub struct AgentCore {
    llm: Arc<dyn LlmClient>,
    fallback_llm: Option<Arc<dyn LlmClient>>,
    tools: Arc<ToolRegistry>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
    /// Cached secret values for redaction
    secrets: Vec<String>,
    audit: Option<Arc<AuditLogger>>,
    skill_registry: Option<Arc<SkillRegistry>>,
    env_probe: EnvironmentProbe,
    workspace_extensions_cache: std::sync::RwLock<Option<WorkspaceExtensionsCache>>,
}

#[derive(Clone)]
struct WorkspaceExtensionsCache {
    cached_at: Instant,
    extensions: HashSet<String>,
}

#[derive(Default)]
struct SkillCandidateContext {
    user_text: String,
    history_text: String,
    combined_text: String,
    recent_file_tokens: HashSet<String>,
    workspace_extensions: HashSet<String>,
}

#[derive(Clone)]
struct SkillCandidateMatch {
    skill: SkillMeta,
    score: i32,
    description_match: bool,
    extension_match: bool,
}

#[derive(Clone, Copy, Default)]
struct MatchSignalScore {
    score: i32,
    matched: bool,
}

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

const RECENT_HISTORY_MESSAGE_LIMIT: usize = 8;
const WORKSPACE_EXTENSION_SCAN_LIMIT: usize = 256;
const WORKSPACE_EXTENSIONS_CACHE_TTL: Duration = Duration::from_secs(60);

impl AgentCore {
    #[cfg(test)]
    fn select_skill_candidates(&self, user_text: &str, history: &[ChatMessage]) -> Vec<SkillMeta> {
        self.select_skill_candidate_matches(user_text, history)
            .into_iter()
            .map(|candidate| candidate.skill)
            .collect()
    }

    fn select_skill_candidate_matches(
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

        let context = self.build_skill_candidate_context(user_text, history);
        let mut scored: Vec<SkillCandidateMatch> = registry
            .list()
            .into_iter()
            .filter_map(|skill| Self::score_skill_candidate(&skill, &context))
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

    fn build_skill_candidate_context(
        &self,
        user_text: &str,
        history: &[ChatMessage],
    ) -> SkillCandidateContext {
        let user_text = user_text.to_lowercase();
        let history_messages: Vec<String> = history
            .iter()
            .rev()
            .filter(|message| message.role != crate::types::Role::System)
            .take(RECENT_HISTORY_MESSAGE_LIMIT)
            .map(|message| message.content.to_lowercase())
            .collect();
        let history_text = history_messages.join("\n");
        let combined_text = if history_text.is_empty() {
            user_text.clone()
        } else {
            format!("{user_text}\n{history_text}")
        };

        SkillCandidateContext {
            recent_file_tokens: extract_file_like_tokens(&combined_text),
            workspace_extensions: self.cached_workspace_extensions(),
            user_text,
            history_text,
            combined_text,
        }
    }

    fn cached_workspace_extensions(&self) -> HashSet<String> {
        if let Ok(cache_guard) = self.workspace_extensions_cache.read()
            && let Some(cache) = cache_guard.as_ref()
            && cache.cached_at.elapsed() < WORKSPACE_EXTENSIONS_CACHE_TTL
        {
            return cache.extensions.clone();
        }

        let extensions = collect_workspace_extensions(
            Path::new(&self.config.app.workspace),
            WORKSPACE_EXTENSION_SCAN_LIMIT,
        );
        let refreshed_cache = WorkspaceExtensionsCache {
            cached_at: Instant::now(),
            extensions: extensions.clone(),
        };

        match self.workspace_extensions_cache.write() {
            Ok(mut cache_guard) => {
                *cache_guard = Some(refreshed_cache);
            }
            Err(poisoned) => {
                *poisoned.into_inner() = Some(refreshed_cache);
            }
        }

        extensions
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

        let description_signal = Self::score_description_match(skill, context);
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

        let extension_signal = Self::score_extension_match(skill, context);
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

    fn score_description_match(
        skill: &SkillMeta,
        context: &SkillCandidateContext,
    ) -> MatchSignalScore {
        let description = skill.normalized_description();
        if description.is_empty() {
            return MatchSignalScore::default();
        }

        let mut score = 0;
        if context.user_text.contains(description) {
            score += 50;
        } else {
            let terms = extract_description_terms(description);
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

    fn score_extension_match(
        skill: &SkillMeta,
        context: &SkillCandidateContext,
    ) -> MatchSignalScore {
        let mut score = 0;

        for extension in skill.extension_terms() {
            if context.combined_text.contains(extension.as_str())
                || context
                    .recent_file_tokens
                    .iter()
                    .any(|token| token.ends_with(extension.as_str()))
            {
                score += 90;
            }

            if context.workspace_extensions.contains(extension.as_str()) {
                score += 15;
            }
        }

        MatchSignalScore {
            score,
            matched: score > 0,
        }
    }

    fn summarize_tool_failures(results: &[crate::types::ToolResult], max_items: usize) -> String {
        let failures: Vec<String> = results
            .iter()
            .filter(|result| {
                result.is_error
                    || (result.output.contains("[exit code:")
                        && !result.output.contains("[exit code: 0]"))
            })
            .filter_map(|result| {
                let mut lines = result.output.lines().filter(|line| !line.trim().is_empty());
                let headline = lines.find(|line| {
                    !line.starts_with("[exit code:")
                        && !line.starts_with("[stdout]")
                        && !line.starts_with("[stderr]")
                })?;

                let cleaned = headline.trim().replace('\r', " ");
                Some(if cleaned.chars().count() > 160 {
                    let truncated: String = cleaned.chars().take(160).collect();
                    format!("{truncated}...")
                } else {
                    cleaned
                })
            })
            .take(max_items)
            .collect();

        if failures.is_empty() {
            return "Multiple tool rounds failed, auto-retry stopped. Check logs or retry manually. / 多轮工具执行失败，已停止自动重试。请检查日志或手动重试。".to_string();
        }

        let bullets = failures
            .iter()
            .enumerate()
            .map(|(index, failure)| format!("{}. {}", index + 1, failure))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "Multiple tool rounds failed, auto-retry stopped. Recent failures: / 多轮工具执行失败，已停止自动重试。最近的失败如下：\n{}\nPlease adjust network, dependencies, or commands based on the errors and try again. / 请根据错误信息调整网络、依赖或命令后再试。",
            bullets
        )
    }

    pub async fn new(
        llm: Arc<dyn LlmClient>,
        fallback_llm: Option<Arc<dyn LlmClient>>,
        tools: Arc<ToolRegistry>,
        memory: Arc<MemoryStore>,
        config: Arc<AppConfig>,
        audit: Option<Arc<AuditLogger>>,
        skill_registry: Option<Arc<SkillRegistry>>,
    ) -> Self {
        let secrets = if config.security.auto_redact_secrets {
            redact::collect_secrets(&config)
        } else {
            vec![]
        };
        let env_probe = EnvironmentProbe::detect(&config.agent).await;
        Self {
            llm,
            fallback_llm,
            tools,
            memory,
            config,
            secrets,
            audit,
            skill_registry,
            env_probe,
            workspace_extensions_cache: std::sync::RwLock::new(None),
        }
    }

    /// Handles an inbound message through the full agentic loop.
    ///
    /// Returns `(OutboundMessage, Vec<ChatMessage>)` — the reply and the full
    /// conversation slice (including tool call rounds) that should be persisted.
    pub async fn handle(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
    ) -> (OutboundMessage, Vec<ChatMessage>) {
        match self.do_handle(msg, history, None).await {
            Ok((reply, messages)) => (reply, messages),
            Err(e) => {
                tracing::error!(error = %e, "agent handle failed / 代理处理失败");
                let reply =
                    OutboundMessage::error(msg, &format!("Processing failed / 处理失败: {e}"));
                (reply, vec![])
            }
        }
    }

    /// Streaming variant — forwards text deltas through `stream_tx`.
    pub async fn handle_streaming(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
        stream_tx: tokio::sync::mpsc::Sender<String>,
    ) -> (OutboundMessage, Vec<ChatMessage>) {
        match self.do_handle(msg, history, Some(stream_tx)).await {
            Ok((reply, messages)) => (reply, messages),
            Err(e) => {
                tracing::error!(error = %e, "agent streaming handle failed / 代理流式处理失败");
                let reply =
                    OutboundMessage::error(msg, &format!("Processing failed / 处理失败: {e}"));
                (reply, vec![])
            }
        }
    }

    async fn do_handle(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
        stream_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> Result<(OutboundMessage, Vec<ChatMessage>)> {
        // 1. Select candidate skills and build the system prompt summary.
        let user_text = msg.content.to_text();
        let candidate_matches = self.select_skill_candidate_matches(&user_text, history);
        let candidate_skills: Vec<SkillMeta> = candidate_matches
            .iter()
            .map(|candidate| candidate.skill.clone())
            .collect();
        let skill_summary = self
            .skill_registry
            .as_ref()
            .map(|registry| {
                registry.build_summary(
                    &candidate_skills,
                    self.config.skills.max_skills_in_prompt,
                    self.config.skills.max_skill_prompt_chars,
                )
            })
            .unwrap_or_default();
        let system_prompt =
            build_system_prompt(&self.config, &skill_summary, &self.env_probe).await;

        // 2. Search relevant memories
        let memories = self
            .memory
            .search_memory(&user_text, self.config.memory.search_limit as usize)
            .await
            .unwrap_or_default();

        // 3. Assemble messages
        let mut messages: Vec<ChatMessage> = Vec::new();

        // System prompt
        messages.push(ChatMessage::system(&system_prompt));

        // Inject relevant memories
        let mem_text = format_memories(&memories);
        if !mem_text.is_empty() {
            messages.push(ChatMessage::system(&mem_text));
        }

        if !candidate_matches.is_empty() {
            tracing::info!(
                skills = ?candidate_matches
                    .iter()
                    .map(|candidate| candidate.skill.name.as_str())
                    .collect::<Vec<_>>(),
                candidate_count = candidate_matches.len(),
                description_matches = ?candidate_matches
                    .iter()
                    .filter(|candidate| candidate.description_match)
                    .map(|candidate| candidate.skill.name.as_str())
                    .collect::<Vec<_>>(),
                extension_matches = ?candidate_matches
                    .iter()
                    .filter(|candidate| candidate.extension_match)
                    .map(|candidate| candidate.skill.name.as_str())
                    .collect::<Vec<_>>(),
                "selected skill candidates from request/history / 已根据请求和历史筛选技能候选"
            );
        } else if let Some(registry) = self.skill_registry.as_ref() {
            let skills_loaded_count = registry.list().len();
            if skills_loaded_count > 0 {
                tracing::debug!(
                    skills_loaded_count,
                    candidate_count = 0,
                    description_matches = ?Vec::<String>::new(),
                    extension_matches = ?Vec::<String>::new(),
                    "skills loaded but no candidates matched this request / 技能已加载，但本次请求未命中任何候选"
                );
            }
        }

        // History (from SQLite)
        messages.extend_from_slice(history);

        // Current user message (with image data if available — borrow, not clone)
        let user_msg = ChatMessage::user_with_images(&user_text, &msg.images);
        messages.push(user_msg);

        if let Some(trimmed) = trim_messages_to_budget(
            &mut messages,
            self.config.limits.max_tokens_per_conversation,
        ) {
            let log_message = if trimmed.trimmed_all_history {
                "all history trimmed — system prompt + user message exceed token budget / 所有历史已裁剪 - 系统提示 + 用户消息超出令牌预算"
            } else {
                "history trimmed to fit token budget / 历史已裁剪以适应令牌预算"
            };
            if trimmed.trimmed_all_history {
                tracing::warn!(
                    trimmed = trimmed.removed_messages,
                    total_est = trimmed.total_tokens,
                    budget = trimmed.budget,
                    "{log_message}"
                );
            } else {
                tracing::info!(
                    trimmed = trimmed.removed_messages,
                    total_est = trimmed.total_tokens,
                    budget = trimmed.budget,
                    "{log_message}"
                );
            }
        }

        // Persist start: user message is always the last element after trimming.
        // Using saturating_sub(1) so persist slice includes the user message onward.
        let persist_start = messages.len().saturating_sub(1);

        // 4. Get tool definitions
        let tool_defs = self.tools.definitions();

        // 5. Agentic loop
        let max_rounds = self.config.agent.max_tool_rounds;
        let mut current_llm: Arc<dyn LlmClient> = self.llm.clone();
        let mut current_fallback: Option<Arc<dyn LlmClient>> = self.fallback_llm.clone();
        let mut current_model_name = self.config.llm.model.clone();
        let mut consecutive_errors: usize = 0;
        let max_consecutive = self.config.agent.max_consecutive_tool_errors as usize;
        for round in 0..max_rounds {
            let llm_start = std::time::Instant::now();
            let response = if stream_tx.is_some() {
                // Streaming path: use chat_stream, forward deltas
                let mut rx = match current_llm.chat_stream(&messages, tool_defs).await {
                    Ok(rx) => rx,
                    Err(e) => {
                        if let Some(ref fallback) = current_fallback {
                            tracing::warn!(error = %e, "primary LLM failed, trying fallback (stream) / 主 LLM 失败，尝试备用模型（流式）");
                            fallback.chat_stream(&messages, tool_defs).await?
                        } else {
                            return Err(e);
                        }
                    }
                };
                let mut partial_text = String::new();
                let mut final_resp = None;
                let mut receiver_dropped = false;
                while let Some(event) = rx.recv().await {
                    match event {
                        StreamEvent::Delta(text) => {
                            partial_text.push_str(&text);
                            if let Some(ref tx) = stream_tx
                                && tx.send(text).await.is_err()
                            {
                                receiver_dropped = true;
                                tracing::info!(
                                    round,
                                    chars = partial_text.chars().count(),
                                    "stream receiver dropped, stopping agent loop early / 流接收端已断开，提前停止 agent 循环"
                                );
                                break;
                            }
                        }
                        StreamEvent::Done(resp) => {
                            final_resp = Some(resp);
                        }
                    }
                }
                if receiver_dropped {
                    crate::types::LlmResponse {
                        text: Some(partial_text),
                        tool_calls: vec![],
                    }
                } else {
                    match final_resp {
                        Some(resp) => resp,
                        None if !partial_text.is_empty() => {
                            tracing::warn!(
                                chars = partial_text.chars().count(),
                                "stream ended without Done event, returning partial text / 流未收到 Done 事件，返回部分文本"
                            );
                            crate::types::LlmResponse {
                                text: Some(partial_text),
                                tool_calls: vec![],
                            }
                        }
                        None => {
                            return Err(anyhow::anyhow!(
                                "stream ended without Done event / 流未收到 Done 事件"
                            ));
                        }
                    }
                }
            } else {
                // Non-streaming path (original)
                match current_llm.chat(&messages, tool_defs).await {
                    Ok(r) => r,
                    Err(e) => {
                        if let Some(ref fallback) = current_fallback {
                            tracing::warn!(error = %e, "primary LLM failed, trying fallback / 主 LLM 失败，尝试备用模型");
                            fallback.chat(&messages, tool_defs).await?
                        } else {
                            return Err(e);
                        }
                    }
                }
            };
            let llm_duration_ms = llm_start.elapsed().as_millis() as u64;

            let has_tool_calls = !response.tool_calls.is_empty();
            let has_text = response.text.is_some();

            // Audit: log LLM call
            if let Some(ref audit) = self.audit
                && self.config.audit.log_llm_calls
            {
                audit.log_llm_call(
                    &msg.trace_id,
                    &msg.chat_id,
                    &current_model_name,
                    messages.len(),
                    has_tool_calls,
                    has_text,
                    llm_duration_ms,
                );
            }

            if has_tool_calls {
                // Record assistant message with tool calls
                messages.push(ChatMessage::assistant_with_tools(
                    response.text.as_deref(),
                    &response.tool_calls,
                ));

                tracing::info!(
                    round,
                    tools = ?response.tool_calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    "executing tool calls / 执行工具调用"
                );

                // Execute all tool calls concurrently.
                let mut results = self.tools.execute_batch(&response.tool_calls).await;

                // Audit: log each tool call (with redaction)
                if let Some(ref audit) = self.audit
                    && self.config.audit.log_tool_calls
                {
                    for (call, result) in response.tool_calls.iter().zip(results.iter()) {
                        // Respect fine-grained audit flags
                        if call.name == "shell_exec" && !self.config.audit.log_shell_commands {
                            continue;
                        }
                        if call.name == "file_write" && !self.config.audit.log_file_writes {
                            continue;
                        }
                        // Redact secrets from tool output before writing to audit log
                        let redacted_output = if self.config.security.auto_redact_secrets {
                            redact::redact_output(
                                &result.output,
                                &self.secrets,
                                &self.config.security.redact_patterns,
                            )
                        } else {
                            result.output.clone()
                        };
                        // Redact secrets from tool arguments
                        let redacted_args = if self.config.security.auto_redact_secrets {
                            let args_str = call.arguments.to_string();
                            let redacted = redact::redact_output(
                                &args_str,
                                &self.secrets,
                                &self.config.security.redact_patterns,
                            );
                            // If redaction broke JSON structure, store as a plain string value
                            // rather than falling back to the unredacted original
                            serde_json::from_str(&redacted)
                                .unwrap_or(serde_json::Value::String(redacted))
                        } else {
                            call.arguments.clone()
                        };
                        audit.log_tool_call(
                            &msg.trace_id,
                            &msg.chat_id,
                            &call.name,
                            &redacted_args,
                            &redacted_output,
                            result.is_error,
                            result.duration_ms,
                        );
                    }
                }

                // Classify tool errors and annotate results with hints
                for (call, result) in response.tool_calls.iter().zip(results.iter_mut()) {
                    let should_classify = result.is_error
                        || (call.name == "shell_exec"
                            && result.output.contains("[exit code:")
                            && !result.output.contains("[exit code: 0]"));
                    if should_classify {
                        let exit_code =
                            crate::tool::error_classifier::parse_exit_code(&result.output);
                        let classification = crate::tool::error_classifier::classify_error(
                            &call.name,
                            &result.output,
                            exit_code,
                            &self.env_probe,
                        );
                        if classification.kind
                            != crate::tool::error_classifier::ToolErrorKind::Unknown
                        {
                            result.output +=
                                &crate::tool::error_classifier::format_error_annotation(
                                    &classification,
                                );
                        }
                    }
                }

                // Append each tool result
                for result in &results {
                    messages.push(ChatMessage::tool_result(result));
                }

                // Consecutive error protection: if majority of tools failed this round,
                // increment counter
                let failed_count = results
                    .iter()
                    .filter(|r| {
                        r.is_error
                            || (r.output.contains("[exit code:")
                                && !r.output.contains("[exit code: 0]"))
                    })
                    .count();
                if failed_count * 2 > results.len() {
                    consecutive_errors += 1;
                    if consecutive_errors >= max_consecutive {
                        let failure_summary = Self::summarize_tool_failures(&results, 3);
                        messages.push(ChatMessage::assistant(&failure_summary));
                        tracing::warn!(
                            consecutive_errors,
                            "consecutive error protection triggered / 连续错误保护已触发"
                        );

                        let reply = OutboundMessage {
                            channel: msg.channel.clone(),
                            chat_id: msg.chat_id.clone(),
                            reply_to: if msg.message_id.is_empty() {
                                None
                            } else {
                                Some(msg.message_id.clone())
                            },
                            content: failure_summary,
                        };
                        let persist_messages = messages[persist_start..].to_vec();
                        return Ok((reply, persist_messages));
                    }
                } else {
                    consecutive_errors = 0;
                }

                // Check if switch_model was called — swap client for subsequent rounds.
                // Security: only trust the signal from the actual switch_model tool,
                // and validate the output is exactly "__switch_model:<known_profile>".
                for (call, result) in response.tool_calls.iter().zip(results.iter()) {
                    if call.name != "switch_model" || result.is_error {
                        continue;
                    }
                    let Some(profile_name) = result.output.strip_prefix("__switch_model:") else {
                        continue;
                    };
                    // Strict validation: profile name must be alphanumeric/dash/underscore only
                    if !profile_name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                    {
                        tracing::warn!(
                            output = %result.output,
                            "switch_model: rejected — profile name contains invalid characters / switch_model: 已拒绝 - 配置名称包含无效字符"
                        );
                        continue;
                    }
                    if let Some(profile_cfg) = self.config.llm_profiles.get(profile_name) {
                        match create_llm_client(profile_cfg) {
                            Ok(new_llm) => {
                                current_llm = new_llm;
                                current_fallback = None;
                                current_model_name = profile_cfg.model.clone();
                                tracing::info!(
                                    profile = profile_name,
                                    model = %current_model_name,
                                    "switched LLM profile mid-session / 会话中已切换 LLM 配置"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    profile = profile_name,
                                    "failed to create LLM client for profile switch / 切换配置时创建 LLM 客户端失败"
                                );
                            }
                        }
                    }
                }

                // Continue loop — let LLM see the results
            } else if has_text {
                // Pure text response — done
                let mut text = response.text.unwrap_or_default();

                // Apply redaction if enabled
                if self.config.security.auto_redact_secrets {
                    text = redact::redact_output(
                        &text,
                        &self.secrets,
                        &self.config.security.redact_patterns,
                    );
                }

                messages.push(ChatMessage::assistant(&text));

                let reply = OutboundMessage {
                    channel: msg.channel.clone(),
                    chat_id: msg.chat_id.clone(),
                    reply_to: if msg.message_id.is_empty() {
                        None
                    } else {
                        Some(msg.message_id.clone())
                    },
                    content: text,
                };

                let persist_messages = messages[persist_start..].to_vec();
                return Ok((reply, persist_messages));
            } else {
                // Empty response — treat as error
                let reply = OutboundMessage::error(
                    msg,
                    "LLM returned an empty response / LLM 返回了空响应",
                );
                let persist_messages = messages[persist_start..].to_vec();
                return Ok((reply, persist_messages));
            }
        }

        // Exceeded max rounds
        let error_text = format!(
            "Exceeded max round limit ({max_rounds} rounds), stopped / 处理超过最大轮次限制 ({max_rounds} 轮)，已停止"
        );
        messages.push(ChatMessage::assistant(&error_text));

        let reply = OutboundMessage::error(msg, &error_text);
        let persist_messages = messages[persist_start..].to_vec();
        Ok((reply, persist_messages))
    }
}

fn extract_file_like_tokens(text: &str) -> HashSet<String> {
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

fn collect_workspace_extensions(workspace: &Path, max_files: usize) -> HashSet<String> {
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

fn extract_description_terms(description: &str) -> HashSet<String> {
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
        if term.len() >= 3 && !DESCRIPTION_STOPWORDS.contains(&term.as_str()) {
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::token_budget::trim_messages_to_budget;
    use super::*;
    use crate::config::AppConfig;
    use crate::types::{LlmResponse, MessageContent, ToolCall, ToolDefinition};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── Mock LLM Client ──────────────────────────────────────────────────────

    /// A mock LLM client that returns responses from a pre-defined sequence.
    struct MockLlm {
        responses: Vec<LlmResponse>,
        stream_events: Option<Vec<StreamEvent>>,
        call_count: AtomicU32,
        last_messages: std::sync::Mutex<Vec<ChatMessage>>,
    }

    impl MockLlm {
        fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses,
                stream_events: None,
                call_count: AtomicU32::new(0),
                last_messages: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn with_stream(events: Vec<StreamEvent>) -> Self {
            Self {
                responses: vec![],
                stream_events: Some(events),
                call_count: AtomicU32::new(0),
                last_messages: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn last_messages(&self) -> Vec<ChatMessage> {
            self.last_messages
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    impl LlmClient for MockLlm {
        fn chat<'a>(
            &'a self,
            messages: &'a [ChatMessage],
            _tools: &'a [ToolDefinition],
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + 'a>> {
            Box::pin(async {
                *self.last_messages.lock().unwrap_or_else(|e| e.into_inner()) = messages.to_vec();
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
                if idx < self.responses.len() {
                    Ok(self.responses[idx].clone())
                } else {
                    // Repeat last response (for max-rounds test)
                    Ok(self.responses.last().unwrap().clone())
                }
            })
        }

        fn chat_stream<'a>(
            &'a self,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDefinition],
        ) -> Pin<
            Box<dyn Future<Output = Result<tokio::sync::mpsc::Receiver<StreamEvent>>> + Send + 'a>,
        > {
            Box::pin(async move {
                if let Some(events) = &self.stream_events {
                    let (tx, rx) = tokio::sync::mpsc::channel(events.len().max(1));
                    let events = events.clone();
                    tokio::spawn(async move {
                        for event in events {
                            let _ = tx.send(event).await;
                        }
                    });
                    Ok(rx)
                } else {
                    let resp = self.chat(&[], &[]).await?;
                    let (tx, rx) = tokio::sync::mpsc::channel(2);
                    if let Some(ref text) = resp.text {
                        let _ = tx.send(StreamEvent::Delta(text.clone())).await;
                    }
                    let _ = tx.send(StreamEvent::Done(resp)).await;
                    Ok(rx)
                }
            })
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    async fn test_memory() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::new(":memory:").await.unwrap())
    }

    fn test_config() -> Arc<AppConfig> {
        let toml_str = r#"
[app]
name = "test"
workspace = "./test_workspace_nonexistent"
log_level = "info"

[feishu]
app_id = "test"
app_secret = "test"

[llm]
provider = "anthropic"
model = "test"
api_key = "test"

[agent]
max_tool_rounds = 3
"#;
        Arc::new(AppConfig::load_from_str(toml_str).unwrap())
    }

    fn test_inbound() -> InboundMessage {
        InboundMessage {
            channel: "test".into(),
            chat_id: "chat_test".into(),
            sender_id: "user_test".into(),
            message_id: "msg_test".into(),
            content: MessageContent::Text("你好".into()),
            timestamp: 0,
            trace_id: String::new(),
            images: vec![],
        }
    }

    fn test_config_with_workspace(workspace: &std::path::Path) -> Arc<AppConfig> {
        let workspace = workspace.to_string_lossy().replace('\\', "/");
        let toml_str = format!(
            r#"
[app]
name = "test"
workspace = "{workspace}"
log_level = "info"

[feishu]
app_id = "test"
app_secret = "test"

[llm]
provider = "anthropic"
model = "test"
api_key = "test"

[agent]
max_tool_rounds = 3
"#
        );
        Arc::new(AppConfig::load_from_str(&toml_str).unwrap())
    }

    fn create_skill_registry(
        root: &std::path::Path,
        skills: &[(&str, &str)],
    ) -> Arc<SkillRegistry> {
        let _ = std::fs::remove_dir_all(root);
        for (name, content) in skills {
            let skill_dir = root.join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        }

        Arc::new(SkillRegistry::scan(
            vec![crate::skill::SkillSource::new(
                "workspace",
                root.to_path_buf(),
            )],
            256 * 1024,
        ))
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_simple_text_response() {
        let memory = test_memory().await;
        let config = test_config();

        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("你好！有什么可以帮你的？".into()),
            tool_calls: vec![],
        }]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (reply, persist) = agent.handle(&test_inbound(), &[]).await;

        assert_eq!(reply.content, "你好！有什么可以帮你的？");
        assert_eq!(reply.channel, "test");
        // persist should contain: user msg + assistant msg
        assert_eq!(persist.len(), 2);
    }

    #[tokio::test]
    async fn test_streaming_returns_partial_text_without_done() {
        let memory = test_memory().await;
        let config = test_config();

        let mock_llm = Arc::new(MockLlm::with_stream(vec![StreamEvent::Delta(
            "处理中断前的部分回复".into(),
        )]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let (reply, persist) = agent.handle_streaming(&test_inbound(), &[], tx).await;

        assert_eq!(reply.content, "处理中断前的部分回复");
        assert_eq!(rx.recv().await.as_deref(), Some("处理中断前的部分回复"));
        assert_eq!(persist.len(), 2);
    }

    #[tokio::test]
    async fn test_cached_workspace_extensions_reuses_recent_scan() {
        let workspace = std::env::temp_dir().join("anqclaw_workspace_extension_cache_test");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("first.txt"), "hello").unwrap();

        let memory = test_memory().await;
        let config = test_config_with_workspace(&workspace);
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let first = agent.cached_workspace_extensions();
        std::fs::write(workspace.join("second.py"), "print('hi')").unwrap();
        let second = agent.cached_workspace_extensions();

        assert!(first.contains(".txt"));
        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn test_trim_messages_to_budget_populates_cached_token_estimates() {
        let mut messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::assistant("older assistant reply"),
            ChatMessage::user("follow-up question from history"),
            ChatMessage::user("current user request"),
        ];
        let total_tokens: usize = messages
            .iter_mut()
            .map(ChatMessage::estimate_tokens_cached)
            .sum();

        for message in &mut messages {
            assert!(message.estimated_tokens().is_some());
        }

        let trimmed = trim_messages_to_budget(&mut messages, total_tokens.saturating_sub(1) as u64)
            .expect("history should be trimmed when budget is smaller than total tokens");

        assert!(trimmed.removed_messages > 0);
        assert_eq!(
            messages.first().map(|message| &message.role),
            Some(&crate::types::Role::System)
        );
        assert_eq!(
            messages.last().map(|message| &message.role),
            Some(&crate::types::Role::User)
        );
        assert!(
            messages
                .iter()
                .all(|message| message.estimated_tokens().is_some())
        );
    }

    #[tokio::test]
    async fn test_streaming_stops_when_receiver_drops() {
        let memory = test_memory().await;
        let config = test_config();

        let mock_llm = Arc::new(MockLlm::with_stream(vec![
            StreamEvent::Delta("第一段".into()),
            StreamEvent::Delta("第二段".into()),
            StreamEvent::Done(LlmResponse {
                text: Some("完整回复".into()),
                tool_calls: vec![],
            }),
        ]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);

        let (reply, persist) = agent.handle_streaming(&test_inbound(), &[], tx).await;

        assert_eq!(reply.content, "第一段");
        assert_eq!(persist.len(), 2);
    }

    #[tokio::test]
    async fn test_tool_call_loop() {
        let memory = test_memory().await;
        let config = test_config();

        let mock_llm = Arc::new(MockLlm::new(vec![
            // Round 1: LLM requests a tool call
            LlmResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({"command": "date"}),
                }],
            },
            // Round 2: LLM sees tool result, returns text
            LlmResponse {
                text: Some("当前时间已获取。".into()),
                tool_calls: vec![],
            },
        ]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (reply, persist) = agent.handle(&test_inbound(), &[]).await;

        assert_eq!(reply.content, "当前时间已获取。");
        // persist: user + assistant(tool_call) + tool_result + assistant(text)
        assert_eq!(persist.len(), 4);
    }

    #[tokio::test]
    async fn test_max_rounds_exceeded() {
        let memory = test_memory().await;
        let config = Arc::new(
            AppConfig::load_from_str(
                r#"
[app]
name = "test"
workspace = "."
log_level = "info"

[feishu]
app_id = "test"
app_secret = "test"

[llm]
provider = "anthropic"
model = "test"
api_key = "test"

[agent]
max_tool_rounds = 3
max_consecutive_tool_errors = 10
"#,
            )
            .unwrap(),
        );

        // Mock always returns tool calls — never a text reply
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_loop".into(),
                name: "shell_exec".into(),
                arguments: serde_json::json!({"command": "echo loop"}),
            }],
        }]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (reply, _persist) = agent.handle(&test_inbound(), &[]).await;

        assert!(reply.content.contains("最大轮次限制"));
    }

    #[tokio::test]
    async fn test_consecutive_errors_triggers_stop() {
        let memory = test_memory().await;
        let config = Arc::new(
            AppConfig::load_from_str(
                r#"
[app]
name = "test"
workspace = "./test_workspace_nonexistent"
log_level = "info"

[feishu]
app_id = "test"
app_secret = "test"

[llm]
provider = "anthropic"
model = "test"
api_key = "test"

[agent]
max_tool_rounds = 10
"#,
            )
            .unwrap(),
        );

        // Mock always returns tool call to a non-existent command (will produce an error result)
        let mock_llm = Arc::new(MockLlm::new(vec![
            // Rounds 1-3: always request tool call → always fails → on round 3 triggers stop hint
            LlmResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_fail".into(),
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({"command": "nonexistent_command_xyz_abc"}),
                }],
            },
        ]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (reply, persist) = agent.handle(&test_inbound(), &[]).await;

        assert!(reply.content.contains("多轮工具执行失败"));
        assert!(!reply.content.contains("最大轮次限制"));

        let has_failure_summary = persist.iter().any(|m| {
            m.role == crate::types::Role::Assistant && m.content.contains("多轮工具执行失败")
        });
        assert!(
            has_failure_summary,
            "failure summary should be persisted after consecutive failures"
        );
    }

    #[tokio::test]
    async fn test_consecutive_errors_resets_on_success() {
        let memory = test_memory().await;
        let config = test_config(); // max_tool_rounds = 3

        let mock_llm = Arc::new(MockLlm::new(vec![
            // Round 1: tool call that fails
            LlmResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_fail1".into(),
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({"command": "nonexistent_command_xyz"}),
                }],
            },
            // Round 2: tool call that succeeds (echo is allowed and will succeed)
            LlmResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_ok".into(),
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({"command": "echo success"}),
                }],
            },
            // Round 3: return text (end loop)
            LlmResponse {
                text: Some("Done.".into()),
                tool_calls: vec![],
            },
        ]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            None,
            vec![],
            None,
        ));
        let agent = AgentCore::new(mock_llm, None, tools, memory, config, None, None).await;

        let (_reply, persist) = agent.handle(&test_inbound(), &[]).await;

        // A success in round 2 should reset the counter, so no stop hint
        let has_stop_hint = persist
            .iter()
            .any(|m| m.content.contains("consecutive rounds of tool failures"));
        assert!(
            !has_stop_hint,
            "stop hint should NOT appear when success resets counter"
        );
    }

    #[tokio::test]
    async fn test_auto_activates_skill_from_recent_history() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_candidate_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_auto_activate_xlsx_skill");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[(
                "xlsx",
                "---\nname: xlsx\ndescription: Spreadsheet skill\nextensions:\n  - .xlsx\nkeywords:\n  - spreadsheet\n---\nUse pandas for spreadsheet inspection.",
            )],
        );

        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("已分析。".into()),
            tool_calls: vec![],
        }]));

        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm.clone(),
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry.clone()),
        )
        .await;

        let msg = InboundMessage {
            content: MessageContent::Text(
                "现在我放进去了。你看下有多少设备，并且对应的基础信息是什么。".into(),
            ),
            ..test_inbound()
        };
        let history = vec![
            ChatMessage::user("给我看下工作区中的 设备数据导出.xlsx 表有多少设备"),
            ChatMessage::assistant("文件 设备数据导出.xlsx 在当前工作区未找到。"),
        ];

        let candidates = agent.select_skill_candidates(&msg.content.to_text(), &history);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "xlsx");

        let (reply, _persist) = agent.handle(&msg, &history).await;

        assert_eq!(reply.content, "已分析。");
        let llm_messages = mock_llm.last_messages();
        let expected_location = skill_registry.find("xlsx").unwrap().prompt_location();
        assert!(llm_messages.iter().any(|message| {
            message.role == crate::types::Role::System
                && message.content.contains("<available_skills>")
                && message.content.contains("<name>xlsx</name>")
                && message.content.contains(&expected_location)
        }));
        assert!(
            !llm_messages
                .iter()
                .any(|message| message.content.contains("# Activated Skill:"))
        );

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_matches_standard_skill_description() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_description_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_description_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[(
                "xlsx",
                "---\nname: xlsx\ndescription: Use this skill any time a spreadsheet file is the primary input or output. Trigger when users want to inspect, edit, or create Excel and tabular files.\n---\nBody",
            )],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates =
            agent.select_skill_candidates("Please inspect this spreadsheet for me", &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "xlsx");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_prefers_keyword_match_over_trigger_match() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_keyword_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_keyword_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[
                (
                    "keyword-skill",
                    "---\nname: keyword-skill\ndescription: keyword\nkeywords:\n  - spreadsheet\npriority: 10\n---\nBody",
                ),
                (
                    "trigger-skill",
                    "---\nname: trigger-skill\ndescription: trigger\ntrigger: spreadsheet\npriority: 10\n---\nBody",
                ),
            ],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates("Please inspect this spreadsheet", &[]);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "keyword-skill");
        assert_eq!(candidates[1].name, "trigger-skill");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_prefers_higher_priority_when_scores_tie() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_priority_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_priority_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[
                (
                    "low-priority",
                    "---\nname: low-priority\ndescription: low\nkeywords:\n  - spreadsheet\npriority: 10\n---\nBody",
                ),
                (
                    "high-priority",
                    "---\nname: high-priority\ndescription: high\nkeywords:\n  - spreadsheet\npriority: 90\n---\nBody",
                ),
            ],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates("Please inspect this spreadsheet", &[]);
        assert_eq!(candidates[0].name, "high-priority");
        assert_eq!(candidates[1].name, "low-priority");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_prefers_extension_signal_over_generic_description() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_extension_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        std::fs::write(workspace_dir.join("report.xlsx"), "fake").unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_extension_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[
                (
                    "generic-description",
                    "---\nname: generic-description\ndescription: Use this skill when users ask for help with spreadsheet analysis.\npriority: 10\n---\nBody",
                ),
                (
                    "xlsx-skill",
                    "---\nname: xlsx-skill\ndescription: xlsx\nextensions:\n  - .xlsx\npriority: 10\n---\nBody",
                ),
            ],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates(
            "Please inspect report.xlsx and help with this spreadsheet",
            &[],
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "xlsx-skill");
        assert_eq!(candidates[1].name, "generic-description");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_ignores_description_stopwords() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_stopwords_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_stopwords_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[(
                "generic-stopwords",
                "---\nname: generic-stopwords\ndescription: the and with this input output\n---\nBody",
            )],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates("Please help me", &[]);
        assert!(candidates.is_empty());

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_matches_chinese_description_phrase() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_chinese_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_chinese_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[(
                "xlsx-cn",
                "---\nname: xlsx-cn\ndescription: 处理电子表格文件\n---\nBody",
            )],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates("帮我处理这个电子表格", &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "xlsx-cn");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_description_does_not_beat_specific_keyword_match() {
        let memory = test_memory().await;
        let workspace_dir =
            std::env::temp_dir().join("anqclaw_test_skill_description_weight_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_description_weight_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[
                (
                    "generic-description",
                    "---\nname: generic-description\ndescription: Use this skill when users want help with spreadsheet files.\npriority: 10\n---\nBody",
                ),
                (
                    "keyword-skill",
                    "---\nname: keyword-skill\ndescription: keyword\nkeywords:\n  - spreadsheet\npriority: 10\n---\nBody",
                ),
            ],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates("Please inspect this spreadsheet", &[]);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "keyword-skill");
        assert_eq!(candidates[1].name, "generic-description");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }

    #[tokio::test]
    async fn test_select_skill_candidates_ignores_disable_model_invocation() {
        let memory = test_memory().await;
        let workspace_dir = std::env::temp_dir().join("anqclaw_test_skill_hidden_workspace");
        let _ = std::fs::remove_dir_all(&workspace_dir);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        std::fs::write(workspace_dir.join("report.xlsx"), "fake").unwrap();
        let config = test_config_with_workspace(&workspace_dir);

        let skill_dir = std::env::temp_dir().join("anqclaw_test_skill_hidden_registry");
        let skill_registry = create_skill_registry(
            &skill_dir,
            &[
                (
                    "hidden-xlsx",
                    "---\nname: hidden-xlsx\ndescription: hidden\nextensions:\n  - .xlsx\ndisable_model_invocation: true\n---\nBody",
                ),
                (
                    "visible-xlsx",
                    "---\nname: visible-xlsx\ndescription: visible\nextensions:\n  - .xlsx\n---\nBody",
                ),
            ],
        );
        let mock_llm = Arc::new(MockLlm::new(vec![LlmResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
        }]));
        let tools = Arc::new(ToolRegistry::new(
            &config.tools,
            &config.security,
            &config.agent,
            memory.clone(),
            Some(skill_registry.clone()),
            vec![],
            Some(&config.skills),
        ));
        let agent = AgentCore::new(
            mock_llm,
            None,
            tools,
            memory,
            config,
            None,
            Some(skill_registry),
        )
        .await;

        let candidates = agent.select_skill_candidates("请看下这个表格", &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "visible-xlsx");

        let _ = std::fs::remove_dir_all(&workspace_dir);
        let _ = std::fs::remove_dir_all(&skill_dir);
    }
}
