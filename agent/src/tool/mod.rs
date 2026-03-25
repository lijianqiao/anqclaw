//! Tool Registry + built-in tools.
//!
//! TODO(future): When splitting into workspace crates, extract the `Tool` trait
//! and `ToolRegistry` into `crates/tool/` and move each built-in tool into its
//! own sub-crate or feature-gated module.

pub mod file;
pub mod memory_tool;
pub mod shell;
pub mod skill_tool;
pub mod web;

use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::{SecuritySection, ToolsSection};
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
}

impl ToolRegistry {
    /// Creates a registry and registers tools based on config toggles.
    pub fn new(
        config: &ToolsSection,
        security: &SecuritySection,
        memory_store: Arc<MemoryStore>,
        skill_registry: Option<Arc<SkillRegistry>>,
    ) -> Self {
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();

        if config.shell_enabled {
            // Merge hardcoded blocked dirs with config blocked dirs
            let mut all_blocked_dirs: Vec<String> = crate::config::HARDCODED_BLOCKED_DIRS
                .iter()
                .map(|s| s.to_string())
                .collect();
            all_blocked_dirs.extend(security.blocked_dirs.iter().cloned());

            let t = shell::ShellExec::new(
                &config.shell_permission_level,
                &config.shell_allowed_commands,
                &config.shell_extra_allowed,
                &config.shell_blocked_commands,
                all_blocked_dirs,
                config.shell_timeout_secs,
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
            let mut all_blocked_dirs: Vec<String> = crate::config::HARDCODED_BLOCKED_DIRS
                .iter()
                .map(|s| s.to_string())
                .collect();
            all_blocked_dirs.extend(security.blocked_dirs.iter().cloned());

            let read_tool = file::FileRead::new(&config.file_access_dir, all_blocked_dirs.clone());
            let write_tool = file::FileWrite::new(&config.file_access_dir, all_blocked_dirs);
            tools.insert(read_tool.name().to_string(), Arc::new(read_tool));
            tools.insert(write_tool.name().to_string(), Arc::new(write_tool));
        }

        if config.memory_tool_enabled {
            let save_tool = memory_tool::MemorySave::new(memory_store.clone());
            let search_tool = memory_tool::MemorySearch::new(
                memory_store,
                config.memory_tool_search_limit,
            );
            tools.insert(save_tool.name().to_string(), Arc::new(save_tool));
            tools.insert(search_tool.name().to_string(), Arc::new(search_tool));
        }

        // Register activate_skill tool if skills are available
        if let Some(registry) = skill_registry {
            if !registry.list().is_empty() {
                let t = skill_tool::ActivateSkill::new(registry);
                tools.insert(t.name().to_string(), Arc::new(t));
            }
        }

        tracing::info!(
            tools = ?tools.keys().collect::<Vec<_>>(),
            "tool registry initialised"
        );

        Self { tools }
    }

    /// Returns tool definitions for all registered tools (sent to the LLM).
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect()
    }

    /// Executes a batch of tool calls concurrently.
    ///
    /// Each call runs in its own spawned task. Failures are captured as
    /// `ToolResult { is_error: true, .. }` so the LLM can see the error and
    /// decide how to proceed.
    pub async fn execute_batch(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        let futs: Vec<_> = calls
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

        futures::future::join_all(futs).await
    }
}
