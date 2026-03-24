use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

use crate::types::{ChatMessage, Role, ToolCall};

// ─── Public Types ────────────────────────────────────────────────────────────

/// A single long-term memory entry retrieved from FTS5 search.
#[derive(Debug, Clone)]
pub struct Memory {
    pub key: String,
    pub content: String,
    pub tags: String,
    pub created_at: i64,
}

// ─── MemoryStore ─────────────────────────────────────────────────────────────

/// Persistent store backed by SQLite.
///
/// Provides:
/// - Conversation history CRUD (messages table)
/// - Long-term memory save + FTS5 full-text search (memories virtual table)
///
/// TODO(future): When splitting into a workspace crate, extract this into
/// `crates/memory/` and expose it via an async trait so other crates
/// (agent, heartbeat) can depend on the abstraction rather than the concrete type.
pub struct MemoryStore {
    pool: SqlitePool,
}

impl MemoryStore {
    /// Opens (or creates) the SQLite database at `db_path` and runs the schema migrations.
    ///
    /// The parent directory is created automatically if it does not exist.
    pub async fn new(db_path: &str) -> Result<Self> {
        // Create parent directory if needed (skip for in-memory dbs)
        if db_path != ":memory:" {
            if let Some(parent) = std::path::Path::new(db_path).parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("create db directory: {}", parent.display()))?;
            }
        }

        let opts = SqliteConnectOptions::from_str(db_path)
            .with_context(|| format!("parse SQLite path: {db_path}"))?
            .create_if_missing(true)
            // WAL mode: concurrent reads + writes, better performance
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // Foreign keys off by default in SQLite — keep off for simplicity
            .foreign_keys(false);

        let pool = SqlitePool::connect_with(opts)
            .await
            .with_context(|| format!("connect to SQLite: {db_path}"))?;

        // Run schema (embedded at compile time — no runtime file I/O needed)
        let schema = include_str!("schema.sql");
        sqlx::raw_sql(schema)
            .execute(&pool)
            .await
            .context("execute schema.sql")?;

        Ok(Self { pool })
    }

    // ── Conversation History ──────────────────────────────────────────────────

    /// Persists a slice of `ChatMessage` for the given `chat_id`.
    ///
    /// `tool_calls` is JSON-serialised; all other fields map 1-to-1.
    pub async fn save_conversation(&self, chat_id: &str, messages: &[ChatMessage]) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut tx = self.pool.begin().await.context("begin transaction")?;

        for msg in messages {
            let role = role_to_str(&msg.role);
            let tool_calls_json = match &msg.tool_calls {
                Some(calls) if !calls.is_empty() => {
                    Some(serde_json::to_string(calls).context("serialise tool_calls")?)
                }
                _ => None,
            };

            sqlx::query(
                r#"
                INSERT INTO messages (chat_id, role, content, tool_calls, tool_call_id, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(chat_id)
            .bind(role)
            .bind(&msg.content)
            .bind(&tool_calls_json)
            .bind(&msg.tool_call_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .context("insert message")?;
        }

        tx.commit().await.context("commit transaction")
    }

    /// Returns the most recent `limit` messages for `chat_id`, ordered oldest-first.
    pub async fn get_history(&self, chat_id: &str, limit: usize) -> Result<Vec<ChatMessage>> {
        // Fetch the last N rows ordered by created_at DESC, then reverse to get
        // chronological order for the LLM context window.
        let rows = sqlx::query(
            r#"
            SELECT role, content, tool_calls, tool_call_id
            FROM messages
            WHERE chat_id = ?
            ORDER BY created_at DESC, id DESC
            LIMIT ?
            "#,
        )
        .bind(chat_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("fetch history")?;

        let mut messages: Vec<ChatMessage> = rows
            .into_iter()
            .map(|row| {
                let role_str: String = row.get("role");
                let content: String = row.get("content");
                let tool_calls_json: Option<String> = row.get("tool_calls");
                let tool_call_id: Option<String> = row.get("tool_call_id");

                let tool_calls: Option<Vec<ToolCall>> = tool_calls_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok());

                ChatMessage {
                    role: str_to_role(&role_str),
                    content,
                    tool_calls,
                    tool_call_id,
                }
            })
            .collect();

        // Reverse so history is chronological (oldest → newest)
        messages.reverse();
        Ok(messages)
    }

    // ── Long-term Memory ──────────────────────────────────────────────────────

    /// Saves a memory entry. If a memory with the same `key` already exists it
    /// is replaced (DELETE + INSERT) to keep keys unique.
    pub async fn save_memory(&self, key: &str, content: &str, tags: &[String]) -> Result<()> {
        let tags_str = tags.join(",");
        let created_at = chrono::Utc::now().timestamp();

        let mut tx = self.pool.begin().await.context("begin transaction")?;

        // Remove existing entry with the same key so the key stays unique.
        sqlx::query("DELETE FROM memories WHERE key = ?")
            .bind(key)
            .execute(&mut *tx)
            .await
            .context("delete old memory")?;

        sqlx::query(
            "INSERT INTO memories (key, content, tags, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(key)
        .bind(content)
        .bind(&tags_str)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .context("insert memory")?;

        tx.commit().await.context("commit transaction")
    }

    /// Full-text searches the `memories` table using FTS5 MATCH syntax.
    ///
    /// Uses the `trigram` tokenizer: queries must be **≥ 3 characters** to produce
    /// a valid trigram. Shorter queries will return an empty result.
    ///
    /// Returns at most `limit` results ranked by relevance (bm25).
    pub async fn search_memory(&self, query: &str, limit: usize) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            r#"
            SELECT key, content, tags, created_at
            FROM memories
            WHERE memories MATCH ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("fts5 search")?;

        Ok(rows
            .into_iter()
            .map(|row| Memory {
                key: row.get("key"),
                content: row.get("content"),
                tags: row.get("tags"),
                created_at: row.get("created_at"),
            })
            .collect())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn str_to_role(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        other => {
            tracing::warn!(role = other, "unknown role in DB, defaulting to User");
            Role::User
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, ToolCall};

    async fn in_memory_store() -> MemoryStore {
        MemoryStore::new(":memory:").await.expect("open :memory: db")
    }

    // ── History tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_and_get_history() {
        let store = in_memory_store().await;
        let chat_id = "chat_001";

        let messages = vec![
            ChatMessage::user("你好"),
            ChatMessage::assistant("你好！有什么可以帮你的？"),
            ChatMessage::user("今天天气怎么样？"),
        ];

        store
            .save_conversation(chat_id, &messages)
            .await
            .expect("save_conversation");

        // Fetch only the most recent 2
        let history = store
            .get_history(chat_id, 2)
            .await
            .expect("get_history");

        assert_eq!(history.len(), 2);
        // Oldest-first order: index 0 should be the second message saved
        assert_eq!(history[0].content, "你好！有什么可以帮你的？");
        assert_eq!(history[1].content, "今天天气怎么样？");
    }

    #[tokio::test]
    async fn test_save_and_get_history_with_tool_calls() {
        let store = in_memory_store().await;
        let chat_id = "chat_tool";

        let tool_call = ToolCall {
            id: "call_abc".to_string(),
            name: "shell_exec".to_string(),
            arguments: serde_json::json!({ "command": "date" }),
        };

        let messages = vec![
            ChatMessage::user("现在几点？"),
            ChatMessage::assistant_with_tools(None, &[tool_call.clone()]),
            ChatMessage::tool_result(&crate::types::ToolResult {
                call_id: "call_abc".to_string(),
                output: "Mon Mar 24 12:00:00 UTC 2026".to_string(),
                is_error: false,
            }),
            ChatMessage::assistant("现在是 2026 年 3 月 24 日 12:00。"),
        ];

        store
            .save_conversation(chat_id, &messages)
            .await
            .expect("save_conversation with tool calls");

        let history = store
            .get_history(chat_id, 10)
            .await
            .expect("get_history");

        assert_eq!(history.len(), 4);

        // Verify tool_calls round-trip
        let assistant_msg = &history[1];
        assert_eq!(assistant_msg.role, Role::Assistant);
        let calls = assistant_msg.tool_calls.as_ref().expect("tool_calls present");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");

        // Verify tool result
        let tool_msg = &history[2];
        assert_eq!(tool_msg.role, Role::Tool);
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_abc"));
    }

    #[tokio::test]
    async fn test_empty_history() {
        let store = in_memory_store().await;

        let history = store
            .get_history("nonexistent_chat", 20)
            .await
            .expect("get_history on empty chat");

        assert!(history.is_empty());
    }

    // ── Memory tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_and_search_memory() {
        let store = in_memory_store().await;

        store
            .save_memory(
                "user_preference_language",
                "用户偏好使用中文回复",
                &["preference".to_string(), "language".to_string()],
            )
            .await
            .expect("save_memory");

        store
            .save_memory(
                "user_timezone",
                "用户时区为 Asia/Shanghai，UTC+8",
                &["preference".to_string(), "timezone".to_string()],
            )
            .await
            .expect("save_memory");

        // trigram tokenizer 要求查询词 >= 3 个字符才能生成 trigram；
        // "中文回复" 会产生 trigram ["中文回", "文回复"]，可命中第一条记忆。
        let results = store
            .search_memory("中文回复", 5)
            .await
            .expect("search_memory");

        assert!(!results.is_empty(), "should find memories matching '中文回复'");
        assert!(
            results.iter().any(|m| m.key == "user_preference_language"),
            "should include language preference"
        );
    }

    #[tokio::test]
    async fn test_save_memory_upsert() {
        let store = in_memory_store().await;

        store
            .save_memory("my_key", "original content", &[])
            .await
            .expect("first save");

        store
            .save_memory("my_key", "updated content", &[])
            .await
            .expect("upsert save");

        let results = store
            .search_memory("updated", 5)
            .await
            .expect("search after upsert");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "updated content");
    }

    #[tokio::test]
    async fn test_empty_memory_search() {
        let store = in_memory_store().await;

        let results = store
            .search_memory("nonexistent_term_xyz", 5)
            .await
            .expect("search on empty table");

        assert!(results.is_empty());
    }
}
