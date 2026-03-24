//! `memory_save` and `memory_search` tools.
//!
//! Thin wrappers around `MemoryStore` so the LLM can persist and retrieve
//! long-term knowledge via tool calls.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::memory::MemoryStore;

use super::Tool;

// ─── memory_save ─────────────────────────────────────────────────────────────

pub struct MemorySave {
    store: Arc<MemoryStore>,
}

impl MemorySave {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `key` parameter"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `content` parameter"))?;

        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        self.store.save_memory(key, content, &tags).await?;

        Ok(format!("Memory saved: key=`{key}`"))
    }
}

impl Tool for MemorySave {
    fn name(&self) -> &str {
        "memory_save"
    }

    fn description(&self) -> &str {
        "Save a piece of information to long-term memory. Use a descriptive key. If a memory with the same key exists, it will be updated."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "A unique descriptive key for this memory (e.g., 'user_birthday', 'project_deadline')"
                },
                "content": {
                    "type": "string",
                    "description": "The content to remember"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional tags for categorisation"
                }
            },
            "required": ["key", "content"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_execute(args))
    }
}

// ─── memory_search ───────────────────────────────────────────────────────────

pub struct MemorySearch {
    store: Arc<MemoryStore>,
    default_limit: u32,
}

impl MemorySearch {
    pub fn new(store: Arc<MemoryStore>, default_limit: u32) -> Self {
        Self {
            store,
            default_limit,
        }
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `query` parameter"))?;

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(self.default_limit as usize);

        let memories = self.store.search_memory(query, limit).await?;

        if memories.is_empty() {
            return Ok("No memories found.".to_string());
        }

        let mut output = String::new();
        for (i, mem) in memories.iter().enumerate() {
            output.push_str(&format!(
                "{}. [{}] {}\n   tags: {}\n",
                i + 1,
                mem.key,
                mem.content,
                if mem.tags.is_empty() { "(none)" } else { &mem.tags },
            ));
        }
        Ok(output)
    }
}

impl Tool for MemorySearch {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search long-term memory using a text query. Returns the most relevant memories ranked by relevance."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)"
                }
            },
            "required": ["query"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_execute(args))
    }
}
