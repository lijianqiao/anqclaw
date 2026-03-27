//! Tool Registry + built-in tools.
//!
//! TODO(future): When splitting into workspace crates, extract the `Tool` trait
//! and `ToolRegistry` into `crates/tool/` and move each built-in tool into its
//! own sub-crate or feature-gated module.

pub mod custom;
pub mod error_classifier;
pub mod file;
pub mod image_info;
pub mod memory_tool;
pub mod model_tool;
pub mod pdf_read;
pub mod shell;
pub mod skill_tool;
pub mod web;

use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::{SecuritySection, SkillsSection, ToolsSection};
use crate::memory::MemoryStore;
use crate::skill::SkillRegistry;
use crate::types::{ToolCall, ToolDefinition, ToolResult};

// ─── Tool Trait ──────────────────────────────────────────────────────────────

/// A tool that can be invoked by the LLM during an agentic loop.
///
/// Object-safe: uses `Pin<Box<dyn Future>>` so we can store `Arc<dyn Tool>`.
pub trait Tool: Send + Sync {
    /// Unique tool name (must match the name in the JSON schema sent to the LLM).
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments.
    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

// ─── Tool Registry ───────────────────────────────────────────────────────────

/// Holds all registered tools and dispatches execution by name.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Cached tool definitions, built once at construction.
    definitions_cache: Vec<ToolDefinition>,
}

impl ToolRegistry {
    /// Creates a registry and registers tools based on config toggles.
    pub fn new(
        config: &ToolsSection,
        security: &SecuritySection,
        agent: &crate::config::AgentSection,
        memory_store: Arc<MemoryStore>,
        skill_registry: Option<Arc<SkillRegistry>>,
        llm_profile_names: Vec<String>,
        skills_config: Option<&SkillsSection>,
    ) -> Self {
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();

        // Merge hardcoded blocked dirs with config blocked dirs (computed once, reused below)
        let all_blocked_dirs: Vec<String> = {
            let mut dirs: Vec<String> = crate::config::HARDCODED_BLOCKED_DIRS
                .iter()
                .map(|s| s.to_string())
                .collect();
            dirs.extend(security.blocked_dirs.iter().cloned());
            dirs
        };

        if config.shell_enabled {
            let venv_path = if agent.auto_install_packages && agent.install_scope == "venv" {
                Some(agent.venv_path.clone())
            } else {
                None
            };
            let t = shell::ShellExec::new(
                &config.shell_permission_level,
                &config.shell_allowed_commands,
                &config.shell_extra_allowed,
                &config.shell_blocked_commands,
                all_blocked_dirs.clone(),
                security.trusted_dirs.clone(),
                config.shell_timeout_secs,
                Some(config.file_access_dir.clone()),
                venv_path,
                Some(agent.managed_python_version.clone()),
            );
            tools.insert(t.name().to_string(), Arc::new(t));
        }

        if config.web_fetch_enabled {
            let t = web::WebFetch::new(
                config.web_fetch_timeout_secs,
                config.web_fetch_max_bytes,
                config.web_blocked_domains.clone(),
            );
            tools.insert(t.name().to_string(), Arc::new(t));
        }

        if config.file_enabled {
            let read_tool = file::FileRead::new(
                &config.file_access_dir,
                all_blocked_dirs.clone(),
                security.trusted_dirs.clone(),
            );
            let write_tool = file::FileWrite::new(
                &config.file_access_dir,
                all_blocked_dirs.clone(),
                security.trusted_dirs.clone(),
            );
            tools.insert(read_tool.name().to_string(), Arc::new(read_tool));
            tools.insert(write_tool.name().to_string(), Arc::new(write_tool));
        }

        if config.memory_tool_enabled {
            let save_tool = memory_tool::MemorySave::new(memory_store.clone());
            let search_tool =
                memory_tool::MemorySearch::new(memory_store, config.memory_tool_search_limit);
            tools.insert(save_tool.name().to_string(), Arc::new(save_tool));
            tools.insert(search_tool.name().to_string(), Arc::new(search_tool));
        }

        // Register activate_skill tool if skills are available
        if let Some(registry) = skill_registry
            && !registry.list().is_empty()
        {
            let max_active = skills_config.map(|s| s.max_active_skills).unwrap_or(3);
            let t = skill_tool::ActivateSkill::new(registry, max_active);
            tools.insert(t.name().to_string(), Arc::new(t));
        }
        // Register switch_model tool if multiple LLM profiles are configured
        if llm_profile_names.len() > 1 {
            let t = model_tool::SwitchModel::new(llm_profile_names);
            tools.insert(t.name().to_string(), Arc::new(t));
        }
        // Register custom tools from config
        for custom_config in &config.custom {
            match custom::CustomTool::new(
                custom_config,
                &config.file_access_dir,
                all_blocked_dirs.clone(),
            ) {
                Ok(t) => {
                    tools.insert(t.name().to_string(), Arc::new(t));
                }
                Err(error) => {
                    tracing::warn!(tool = custom_config.name.as_str(), error = %error, "skipping invalid custom tool configuration");
                }
            }
        }

        // Register pdf_read tool
        if config.pdf_read_enabled {
            let t = pdf_read::PdfRead::new(
                &config.file_access_dir,
                all_blocked_dirs.clone(),
                security.trusted_dirs.clone(),
                config.pdf_read_max_chars,
            );
            tools.insert(t.name().to_string(), Arc::new(t));
        }

        // Register image_info tool
        if config.image_info_enabled {
            let t = image_info::ImageInfo::new(
                &config.file_access_dir,
                all_blocked_dirs.clone(),
                security.trusted_dirs.clone(),
            );
            tools.insert(t.name().to_string(), Arc::new(t));
        }

        tracing::info!(
            tools = ?tools.keys().collect::<Vec<_>>(),
            "tool registry initialised"
        );

        let definitions_cache = tools
            .values()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();

        Self {
            tools,
            definitions_cache,
        }
    }

    /// Returns tool definitions for all registered tools (sent to the LLM).
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions_cache
    }

    /// Maximum number of tool calls executed concurrently per batch.
    /// Prevents resource exhaustion if the LLM returns many tool calls at once.
    const MAX_CONCURRENT_TOOLS: usize = 8;

    /// Executes a batch of tool calls with bounded concurrency.
    ///
    /// At most `MAX_CONCURRENT_TOOLS` calls run simultaneously. Failures are
    /// captured as `ToolResult { is_error: true, .. }` so the LLM can see the
    /// error and decide how to proceed.
    ///
    /// Results are returned in the **same order** as the input `calls`.
    pub async fn execute_batch(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        use futures::stream::{FuturesOrdered, StreamExt};

        // Pre-resolve tools and build futures (not yet polled)
        let mut pending: std::collections::VecDeque<_> = calls
            .iter()
            .map(|call| {
                let call_id = call.id.clone();
                let tool = self.tools.get(&call.name).cloned();
                let args = call.arguments.clone();
                let name = call.name.clone();

                async move {
                    match tool {
                        Some(t) => match t.execute(args).await {
                            Ok(output) => ToolResult {
                                call_id,
                                output,
                                is_error: false,
                            },
                            Err(e) => ToolResult {
                                call_id,
                                output: format!("Tool `{name}` failed: {e}"),
                                is_error: true,
                            },
                        },
                        None => ToolResult {
                            call_id,
                            output: format!("Unknown tool: `{name}`"),
                            is_error: true,
                        },
                    }
                }
            })
            .collect();

        let mut in_flight = FuturesOrdered::new();
        let mut results = Vec::with_capacity(calls.len());

        // Seed initial batch up to concurrency limit
        while in_flight.len() < Self::MAX_CONCURRENT_TOOLS {
            if let Some(fut) = pending.pop_front() {
                in_flight.push_back(fut);
            } else {
                break;
            }
        }

        // As each completes, feed the next pending future
        while let Some(result) = in_flight.next().await {
            results.push(result);
            if let Some(fut) = pending.pop_front() {
                in_flight.push_back(fut);
            }
        }

        results
    }
}
