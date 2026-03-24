-- ANQ Agent — SQLite Schema
-- messages: 对话历史（含 tool call 中间过程）
-- memories: 长期记忆（FTS5 全文检索）

CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id      TEXT    NOT NULL,
    role         TEXT    NOT NULL,       -- system / user / assistant / tool
    content      TEXT    NOT NULL,
    tool_calls   TEXT,                   -- JSON array of ToolCall, nullable
    tool_call_id TEXT,                   -- tool result 对应的 call id, nullable
    created_at   INTEGER NOT NULL        -- Unix timestamp (seconds)
);

CREATE INDEX IF NOT EXISTS idx_messages_chat_time
    ON messages (chat_id, created_at);

-- FTS5 虚拟表：长期记忆全文检索
-- tokenize = "trigram": 按 3 字符滑窗切分，天然支持中文子串匹配。
-- 代价：索引略大；查询词需 >= 3 个字符。
-- created_at 标记 UNINDEXED 以节省索引空间。
CREATE VIRTUAL TABLE IF NOT EXISTS memories USING fts5 (
    key,
    content,
    tags,
    created_at UNINDEXED,
    tokenize = "trigram"
);
