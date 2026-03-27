-- anqclaw — SQLite Schema
-- messages: 对话历史（含 tool call 中间过程）
-- memories: 长期记忆（普通表作为 source of truth，FTS5 作为全文索引）

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id TEXT NOT NULL,
    role TEXT NOT NULL, -- system / user / assistant / tool
    content TEXT NOT NULL,
    tool_calls TEXT, -- JSON array of ToolCall, nullable
    tool_call_id TEXT, -- tool result 对应的 call id, nullable
    created_at INTEGER NOT NULL -- Unix timestamp (seconds)
);

CREATE INDEX IF NOT EXISTS idx_messages_chat_time ON messages (chat_id, created_at);

CREATE TABLE IF NOT EXISTS memories_data (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key TEXT NOT NULL UNIQUE,
    content TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);

-- FTS5 虚拟表：长期记忆全文检索镜像
-- tokenize = "trigram": 按 3 字符滑窗切分，天然支持中文子串匹配。
-- content='memories_data'：FTS 只负责索引，源数据仍以普通表为准。
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5 (
    key,
    content,
    tags,
    content = 'memories_data',
    content_rowid = 'id',
    tokenize = "trigram"
);

CREATE TRIGGER IF NOT EXISTS memories_data_ai AFTER INSERT ON memories_data BEGIN
    INSERT INTO memories_fts(rowid, key, content, tags)
    VALUES (new.id, new.key, new.content, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS memories_data_ad AFTER DELETE ON memories_data BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, key, content, tags)
    VALUES ('delete', old.id, old.key, old.content, old.tags);
END;

CREATE TRIGGER IF NOT EXISTS memories_data_au AFTER UPDATE ON memories_data BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, key, content, tags)
    VALUES ('delete', old.id, old.key, old.content, old.tags);
    INSERT INTO memories_fts(rowid, key, content, tags)
    VALUES (new.id, new.key, new.content, new.tags);
END;