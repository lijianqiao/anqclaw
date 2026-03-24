# ANQ Agent v1 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Rust 构建一个飞书 WebSocket 驱动的私人 AI 助理，支持多 LLM、tool calling、持久记忆和定时任务。

**Architecture:** 单 crate 模块化设计（预留 workspace 拆分）。飞书 WS 接收消息 → Gateway 路由 → AgentCore agentic loop（LLM + tools）→ 飞书 REST API 回复。SQLite 持久化对话历史和长期记忆。

**Tech Stack:** Rust (edition 2024), tokio, tokio-tungstenite, reqwest, sqlx (SQLite), serde, tracing, clap

**设计规格书:** `docs/superpowers/specs/2026-03-24-anq-agent-v1-design.md`

**进度追踪:** `docs/superpowers/plans/2026-03-24-anq-agent-v1-progress.md`

---

## 阶段总览

| 阶段 | 内容 | 对应设计章节 |
|------|------|-------------|
| Phase 1 | 项目脚手架 + 配置 + 公共类型 | §2, §3, §11, §13 |
| Phase 2 | Memory Store（SQLite） | §9 |
| Phase 3 | LLM 抽象层 + 两个 Client | §4, §15 |
| Phase 4 | Tool Registry + 6 个内置工具 | §8 |
| Phase 5 | Agent Core — Agentic Loop | §7 |
| Phase 6 | 飞书 Channel 实现 | §5 |
| Phase 7 | Gateway 消息路由 | §6 |
| Phase 8 | Heartbeat 定时任务 | §10 |
| Phase 9 | 主入口 + Workspace + 优雅关机 | §12, §16 |
| Phase 10 | 集成测试 + 端到端验证 | 全部 |

---

## Phase 1: 项目脚手架 + 配置 + 公共类型

**目标：** 建立项目骨架，能 `cargo build` 通过，配置能加载，公共类型就绪。

**Files:**
- Modify: `agent/Cargo.toml`
- Create: `agent/src/config.rs`
- Create: `agent/src/types.rs`
- Modify: `agent/src/main.rs`
- Create: `agent/config.toml`

### Task 1.1: 更新 Cargo.toml 依赖

- [ ] **Step 1: 写入完整依赖**

```toml
[package]
name = "anq-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
tokio = { version = "1.50", features = ["full"] }
tokio-tungstenite = { version = "0.29", features = ["rustls-tls-native-roots"] }
reqwest = { version = "0.13", features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }
toml = "1.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
anyhow = "1"
thiserror = "2"
clap = { version = "4.6", features = ["derive"] }
secrecy = "0.10"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
dashmap = "6"
lru = "0.13"
futures = "0.3"
```

- [ ] **Step 2: 运行 `cargo check` 确认依赖解析成功**

Run: `cd agent && cargo check`
Expected: 编译通过（可能有 unused import 警告，正常）

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add all v1 dependencies"
```

### Task 1.2: 创建公共类型 types.rs

- [ ] **Step 1: 创建 `src/types.rs`**

实现以下类型（全部需要 `derive(Debug, Clone)` 和必要的 `Serialize/Deserialize`）：
- `InboundMessage` + `InboundMessage::heartbeat()` 构造
- `MessageContent` 枚举 + `MessageContent::to_text()` 方法
- `OutboundMessage` + `OutboundMessage::error()` 构造
- `ChatMessage` + 构造方法：`system()`, `user()`, `assistant()`, `assistant_with_tools()`, `tool_result()`
- `Role` 枚举
- `ToolCall`, `ToolResult`, `ToolDefinition`
- `LlmResponse` struct（`text: Option<String>`, `tool_calls: Vec<ToolCall>`）

参考设计规格书 §3 和 §4 中的类型定义。

- [ ] **Step 2: 在 `main.rs` 中 `mod types;` 引入，运行 `cargo check`**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/types.rs src/main.rs
git commit -m "feat: add core types - InboundMessage, ChatMessage, LlmResponse, etc."
```

### Task 1.3: 创建配置模块 config.rs

- [ ] **Step 1: 创建 `src/config.rs`**

实现 `AppConfig` 及各子结构体（全部 `#[derive(Deserialize)]`）：
- `AppSection`: name, workspace, log_level
- `FeishuSection`: app_id, app_secret (SecretString), allow_from
- `LlmSection`: provider, model, api_key (SecretString), base_url, max_tokens, temperature
- `ToolsSection`: 各工具的 enabled/timeout/limit 配置
- `MemorySection`: db_path, history_limit, search_limit
- `HeartbeatSection`: enabled, interval_minutes, notify_channel, notify_chat_id
- `AgentSection`: max_tool_rounds, system_prompt_file

每个可选字段使用 `#[serde(default = "...")]` 提供默认值。
实现 `AppConfig::load(path: &str) -> Result<Self>` 方法，读取 TOML 文件并解析。
支持环境变量覆盖（参考设计规格书 §14）：
- 加载 TOML 后，检查敏感字段是否以 `${` 开头（如 `api_key = "${ANQ_LLM_API_KEY}"`），如果是则读取对应环境变量替换
- 同时支持直接在 TOML 中写明文值（开发/测试环境使用）
- 需要覆盖的字段：`llm.api_key` → `ANQ_LLM_API_KEY`，`feishu.app_secret` → `ANQ_FEISHU_APP_SECRET`

- [ ] **Step 2: 创建 `config.toml` 示例配置文件**

包含所有 section 和注释说明（参考设计规格书 §11）。

- [ ] **Step 3: 在 `main.rs` 中加载配置并打印确认**

```rust
mod config;
mod types;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::AppConfig::load("config.toml")?;
    println!("Loaded config: {}", config.app.name);
    Ok(())
}
```

- [ ] **Step 4: 运行 `cargo run` 确认配置加载成功**

Run: `cargo run`
Expected: 输出 "Loaded config: anq-agent"

- [ ] **Step 5: Commit**

```bash
git add src/config.rs config.toml src/main.rs
git commit -m "feat: add TOML config loading with defaults and env var override"
```

### Task 1.4: 初始化 tracing 日志

- [ ] **Step 1: 在 `main.rs` 中初始化 tracing-subscriber**

根据 `config.app.log_level` 初始化 `EnvFilter`，设置 JSON 格式输出到 stderr。

- [ ] **Step 2: 验证日志输出**

Run: `cargo run`
Expected: 能看到 tracing 初始化日志

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: init tracing subscriber with configurable log level"
```

---

## Phase 2: Memory Store（SQLite）

**目标：** SQLite 数据库初始化、建表、对话历史 CRUD、长期记忆 FTS5 搜索。

**Files:**
- Create: `agent/src/memory/mod.rs`
- Create: `agent/src/memory/schema.sql`

### Task 2.1: 创建 schema.sql

- [ ] **Step 1: 创建 `src/memory/schema.sql`**

```sql
CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id      TEXT NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    tool_calls   TEXT,
    tool_call_id TEXT,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_chat_time ON messages(chat_id, created_at);

CREATE VIRTUAL TABLE IF NOT EXISTS memories USING fts5(
    key, content, tags, created_at UNINDEXED
);
```

- [ ] **Step 2: Commit**

```bash
git add src/memory/schema.sql
git commit -m "feat: add SQLite schema for messages and memories tables"
```

### Task 2.2: 实现 MemoryStore

- [ ] **Step 1: 创建 `src/memory/mod.rs`**

实现：
- `MemoryStore::new(db_path: &str) -> Result<Self>`: 连接 SQLite，自动创建 db 文件和目录，执行 schema.sql 建表
- `save_conversation(chat_id, messages: &[ChatMessage]) -> Result<()>`: 批量 INSERT，`tool_calls` 字段 JSON 序列化
- `get_history(chat_id, limit) -> Result<Vec<ChatMessage>>`: SELECT 最近 N 条，按 created_at ASC 排序
- `save_memory(key, content, tags) -> Result<()>`: INSERT INTO memories，`created_at` 使用 `strftime('%s','now')`
- `search_memory(query, limit) -> Result<Vec<Memory>>`: FTS5 MATCH 查询

定义 `Memory` 结构体：`key, content, tags, created_at`。

- [ ] **Step 2: 在 `main.rs` 中 `mod memory;`，运行 `cargo check`**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: 编写单元测试**

在 `src/memory/mod.rs` 底部 `#[cfg(test)] mod tests` 中：
- `test_save_and_get_history`: 保存 3 条消息，get_history limit=2 验证返回最近 2 条
- `test_save_and_search_memory`: 保存记忆，搜索关键词验证匹配
- `test_empty_history`: 查询不存在的 chat_id 返回空 Vec

使用 SQLite `:memory:` 作为测试数据库。

- [ ] **Step 4: 运行测试**

Run: `cargo test memory`
Expected: 3 个测试全部通过

- [ ] **Step 5: Commit**

```bash
git add src/memory/
git commit -m "feat: implement MemoryStore with SQLite - history CRUD and FTS5 memory search"
```

---

## Phase 3: LLM 抽象层 + 两个 Client

**目标：** LlmClient trait 和 Anthropic / OpenAI-compatible 两个实现。

**Files:**
- Create: `agent/src/llm/mod.rs`
- Create: `agent/src/llm/anthropic.rs`
- Create: `agent/src/llm/openai_compat.rs`

### Task 3.1: 定义 LlmClient trait

- [ ] **Step 1: 创建 `src/llm/mod.rs`**

定义 `LlmClient` trait（参考设计规格书 §4）：

```rust
pub trait LlmClient: Send + Sync {
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> impl Future<Output = Result<LlmResponse>> + Send;
}
```

注意：由于需要 `dyn LlmClient`（在 AgentCore 中使用 `Arc<dyn LlmClient>`），需要手动实现 object-safe 版本。使用 `Box<dyn Future>` 方式：

```rust
pub trait LlmClient: Send + Sync {
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse>> + Send + '_>>;
}
```

导出 `pub mod anthropic;` 和 `pub mod openai_compat;`。

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/llm/mod.rs
git commit -m "feat: define LlmClient trait with object-safe async interface"
```

### Task 3.2: 实现 OpenAI-compatible Client

- [ ] **Step 1: 创建 `src/llm/openai_compat.rs`**

实现 `OpenAiCompatClient`：
- `new(config: &LlmSection) -> Self`: 初始化 reqwest::Client
- 实现 `LlmClient::chat()`:
  - 构建 `POST {base_url}/v1/chat/completions` 请求
  - 将 `ChatMessage` 映射到 OpenAI messages 格式（system/user/assistant/tool role）
  - 将 `ToolDefinition` 映射到 OpenAI tools 格式（function type）
  - 解析响应：提取 `message.content` → `text`，`message.tool_calls` → `tool_calls`
  - 实现重试逻辑（§15）：429/500 指数退避，最多 3 次

内部请求/响应类型用私有结构体定义，不暴露到模块外。

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/llm/openai_compat.rs
git commit -m "feat: implement OpenAI-compatible LLM client with retry logic"
```

### Task 3.3: 实现 Anthropic Client

- [ ] **Step 1: 创建 `src/llm/anthropic.rs`**

实现 `AnthropicClient`：
- `new(config: &LlmSection) -> Self`
- 实现 `LlmClient::chat()`:
  - 构建 `POST https://api.anthropic.com/v1/messages` 请求
  - **特殊映射**：
    - `Role::System` 消息提取到请求的 `system` 字段（非 messages 数组）
    - `Role::Tool` 消息映射为 `tool_result` content block
    - assistant 消息中的 `tool_calls` 映射为 `tool_use` content block
  - Header: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`
  - 解析响应：遍历 `content` 数组，text block → `text`，tool_use block → `tool_calls`
  - 同样实现重试逻辑，额外处理 529 (Overload)

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/llm/anthropic.rs
git commit -m "feat: implement Anthropic Claude API client with content block mapping"
```

### Task 3.4: LLM Client 工厂函数

- [ ] **Step 1: 在 `src/llm/mod.rs` 添加工厂函数**

```rust
pub fn create_llm_client(config: &LlmSection) -> Arc<dyn LlmClient> {
    match config.provider.as_str() {
        "anthropic" => Arc::new(AnthropicClient::new(config)),
        "openai_compat" => Arc::new(OpenAiCompatClient::new(config)),
        other => panic!("Unknown LLM provider: {other}"),
    }
}
```

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/llm/mod.rs
git commit -m "feat: add LLM client factory function for provider selection"
```

---

## Phase 4: Tool Registry + 6 个内置工具

**目标：** Tool trait、ToolRegistry、6 个工具实现。

**Files:**
- Create: `agent/src/tool/mod.rs`
- Create: `agent/src/tool/shell.rs`
- Create: `agent/src/tool/web.rs`
- Create: `agent/src/tool/file.rs`
- Create: `agent/src/tool/memory_tool.rs`

### Task 4.1: Tool trait + ToolRegistry

- [ ] **Step 1: 创建 `src/tool/mod.rs`**

定义 `Tool` trait（object-safe，使用 `Pin<Box<dyn Future>>`）和 `ToolRegistry`：
- `ToolRegistry::new()`: 空注册表
- `register(tool: Arc<dyn Tool>)`: 注册工具
- `definitions() -> Vec<ToolDefinition>`: 返回所有工具的 JSON Schema 定义
- `execute_batch(calls: &[ToolCall]) -> Vec<ToolResult>`: 用 `futures::future::join_all` 并发执行，单个失败返回 `ToolResult { is_error: true }`

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/tool/mod.rs
git commit -m "feat: define Tool trait and ToolRegistry with concurrent batch execution"
```

### Task 4.2: shell_exec 工具

- [ ] **Step 1: 创建 `src/tool/shell.rs`**

实现 `ShellExecTool`：
- `new(config: &ToolsSection)`: 保存白名单列表和超时时间
- `parameters_schema()`: `{ "command": { "type": "string", "description": "..." } }`
- `execute()`:
  1. 解析 args 中的 `command` 字符串
  2. 拆分命令名和参数（`command.split_whitespace()`，首个元素为命令名）
  3. 检查命令名是否在 `shell_allowed_commands` 白名单
  4. 使用 `tokio::process::Command::new(cmd).args(args)` 执行，**不经过 shell**
  5. 使用 `tokio::time::timeout` 实现超时
  6. 返回 stdout + stderr 拼接

- [ ] **Step 2: 编写测试**

- `test_allowed_command`: 执行 `echo hello`（假设 echo 在白名单），验证输出
- `test_blocked_command`: 执行不在白名单的命令，验证返回 is_error

- [ ] **Step 3: 运行测试**

Run: `cargo test shell`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add src/tool/shell.rs
git commit -m "feat: implement shell_exec tool with command whitelist and timeout"
```

### Task 4.3: web_fetch 工具

- [ ] **Step 1: 创建 `src/tool/web.rs`**

实现 `WebFetchTool`：
- `new(config: &ToolsSection)`: 保存超时和 max_bytes
- `execute()`:
  1. 提取 `url` 参数
  2. `reqwest::Client` 带超时和最多 5 次重定向
  3. 检查 Content-Type：JSON 直接返回，二进制返回错误
  4. 读取 body，截断到 max_bytes
  5. 简单 strip HTML 标签（正则 `<[^>]+>` 替换为空）

- [ ] **Step 2: 编写测试**（可 mock 或用已知 URL）

- [ ] **Step 3: Commit**

```bash
git add src/tool/web.rs
git commit -m "feat: implement web_fetch tool with HTML stripping and size limit"
```

### Task 4.4: file_read + file_write 工具

- [ ] **Step 1: 创建 `src/tool/file.rs`**

实现 `FileReadTool` 和 `FileWriteTool`：
- 共享路径校验函数 `validate_path(path: &str, access_dir: &str) -> Result<PathBuf>`：
  1. `std::fs::canonicalize()` 或手动 resolve
  2. 检查 canonicalized path 以 `access_dir` 开头
  3. 不通过则返回错误
- `FileReadTool::execute()`: validate_path → `tokio::fs::read_to_string`
- `FileWriteTool::execute()`: validate_path → `tokio::fs::write`

- [ ] **Step 2: 编写测试**

- `test_read_valid_path`: 创建临时文件，读取验证
- `test_write_valid_path`: 写入后读取验证
- `test_path_traversal_blocked`: 尝试 `../../../etc/passwd` 路径，验证拒绝

- [ ] **Step 3: 运行测试**

Run: `cargo test file`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add src/tool/file.rs
git commit -m "feat: implement file_read/file_write tools with path traversal protection"
```

### Task 4.5: memory_save + memory_search 工具

- [ ] **Step 1: 创建 `src/tool/memory_tool.rs`**

实现 `MemorySaveTool` 和 `MemorySearchTool`：
- 两者都持有 `Arc<MemoryStore>` 引用
- `MemorySaveTool::execute()`: 提取 key/content/tags，调用 `memory.save_memory()`
- `MemorySearchTool::execute()`: 提取 query/limit，调用 `memory.search_memory()`，格式化结果为文本

- [ ] **Step 2: 编写测试**（使用 in-memory SQLite）

- [ ] **Step 3: Commit**

```bash
git add src/tool/memory_tool.rs
git commit -m "feat: implement memory_save and memory_search tools"
```

### Task 4.6: 工具注册工厂

- [ ] **Step 1: 在 `src/tool/mod.rs` 添加 `create_tool_registry()` 函数**

```rust
pub fn create_tool_registry(
    config: &ToolsSection,
    memory: Arc<MemoryStore>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    if config.shell_enabled {
        registry.register(Arc::new(ShellExecTool::new(config)));
    }
    if config.web_fetch_enabled {
        registry.register(Arc::new(WebFetchTool::new(config)));
    }
    if config.file_enabled {
        registry.register(Arc::new(FileReadTool::new(config)));
        registry.register(Arc::new(FileWriteTool::new(config)));
    }
    if config.memory_tool_enabled {
        registry.register(Arc::new(MemorySaveTool::new(memory.clone())));
        registry.register(Arc::new(MemorySearchTool::new(memory, config)));
    }
    registry
}
```

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/tool/mod.rs
git commit -m "feat: add tool registry factory with config-driven registration"
```

---

## Phase 5: Agent Core — Agentic Loop

**目标：** 实现 AgentCore，能执行完整的 LLM ↔ tool 循环。

**Files:**
- Create: `agent/src/agent/mod.rs`
- Create: `agent/src/agent/context.rs`
- Create: `agent/src/agent/prompt.rs`

### Task 5.1: System prompt 构建（context.rs + prompt.rs）

- [ ] **Step 1: 创建 `src/agent/prompt.rs`**

定义内置默认 system prompt 常量字符串。

- [ ] **Step 2: 创建 `src/agent/context.rs`**

实现 `build_system_prompt(config: &AppConfig) -> String`：
1. 读取 workspace 文件（SOUL.md → AGENTS.md → TOOLS.md → USER.md → MEMORY.md），按顺序拼接
2. 文件不存在则跳过，不报错
3. 如果 `config.agent.system_prompt_file` 非空，则使用该文件内容替代
4. 都不存在则使用 `prompt.rs` 中的默认 prompt

实现 `format_memories(memories: &[Memory]) -> String`：
将记忆列表格式化为 `[记忆] key: content` 文本块。

- [ ] **Step 3: 运行 `cargo check`**

- [ ] **Step 4: Commit**

```bash
git add src/agent/prompt.rs src/agent/context.rs
git commit -m "feat: implement system prompt builder from workspace markdown files"
```

### Task 5.2: AgentCore agentic loop

- [ ] **Step 1: 创建 `src/agent/mod.rs`**

实现 `AgentCore`（参考设计规格书 §7）：
- `new(llm, tools, memory, config) -> Self`
- `handle(msg, history) -> (OutboundMessage, Vec<ChatMessage>)`:
  1. 调用 `build_system_prompt` 构建 system prompt
  2. 调用 `memory.search_memory` 搜索相关记忆
  3. 拼装 messages：system + memories + history + user msg
  4. 循环最多 `config.agent.max_tool_rounds` 轮：
     - 调用 `llm.chat(messages, tool_defs)`
     - tool_calls 为空 → 返回文本回复
     - tool_calls 非空 → `tools.execute_batch()` → 追加结果 → 继续循环
  5. 超过轮次 → 返回错误
  6. 返回 `(OutboundMessage, messages)` 元组

- [ ] **Step 2: 编写单元测试**

使用 mock LlmClient：
- `test_simple_text_response`: mock 返回纯文本，验证直接回复
- `test_tool_call_loop`: mock 先返回 tool call，再返回文本，验证 loop 正确执行
- `test_max_rounds_exceeded`: mock 永远返回 tool calls，验证达到 max_rounds 后停止

- [ ] **Step 3: 运行测试**

Run: `cargo test agent`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add src/agent/
git commit -m "feat: implement AgentCore agentic loop with tool calling"
```

---

## Phase 6: 飞书 Channel 实现

**目标：** 飞书 REST API 封装 + WebSocket 连接管理 + FeishuChannel 实现。

**Files:**
- Create: `agent/src/channel/mod.rs`
- Create: `agent/src/channel/feishu/mod.rs`
- Create: `agent/src/channel/feishu/types.rs`
- Create: `agent/src/channel/feishu/api.rs`
- Create: `agent/src/channel/feishu/ws.rs`

### Task 6.1: Channel trait + 飞书事件类型

- [ ] **Step 1: 创建 `src/channel/mod.rs`**

定义 `Channel` trait（object-safe）：

```rust
pub trait Channel: Send + Sync + 'static {
    fn start(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    fn send_message(
        &self,
        msg: OutboundMessage,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    fn name(&self) -> &str;
}
```

- [ ] **Step 2: 创建 `src/channel/feishu/types.rs`**

定义飞书 WebSocket 事件 JSON 反序列化结构体：
- `FeishuWsEvent`: 顶层事件包装
- `FeishuMessageEvent`: `im.message.receive_v1` 事件体
- `FeishuMessageContent`: 消息内容（text/image/file/post）
- 实现 `FeishuMessageEvent → InboundMessage` 转换

- [ ] **Step 3: Commit**

```bash
git add src/channel/mod.rs src/channel/feishu/types.rs
git commit -m "feat: define Channel trait and Feishu event types"
```

### Task 6.2: 飞书 REST API 封装

- [ ] **Step 1: 创建 `src/channel/feishu/api.rs`**

实现 `FeishuApi`（参考设计规格书 §5）：
- `new(config: &FeishuSection) -> Self`
- `ensure_token() -> Result<String>`: RwLock 缓存 token，过期前 5 分钟刷新
- `get_ws_endpoint() -> Result<String>`: `POST /callback/ws/endpoint`
- `send_text(chat_id, text) -> Result<()>`: `POST /im/v1/messages`
- `reply_text(message_id, text) -> Result<()>`: `POST /im/v1/messages/{id}/reply`

Token 刷新：`POST https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal`

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/channel/feishu/api.rs
git commit -m "feat: implement Feishu REST API client with token auto-refresh"
```

### Task 6.3: 飞书 WebSocket 连接管理

- [ ] **Step 1: 创建 `src/channel/feishu/ws.rs`**

实现 WebSocket 连接循环：
- `connect_and_listen(api, tx, allow_from) -> Result<()>`:
  1. 调用 `api.get_ws_endpoint()` 获取 WS URL
  2. `tokio_tungstenite::connect_async()` 连接
  3. 消息循环：
     - Text message → 解析为 `FeishuWsEvent` → 白名单检查 → 转为 `InboundMessage` → `tx.send()`
     - Ping → 回复 Pong
  4. 连接断开 → 返回错误触发重连

- `run_with_reconnect(api, tx, allow_from)`:
  - 外层循环：调用 `connect_and_listen`
  - 失败后指数退避重连（1s, 2s, 4s, 8s... 最大 60s）
  - 成功连接后重置退避计数

- [ ] **Step 2: Commit**

```bash
git add src/channel/feishu/ws.rs
git commit -m "feat: implement Feishu WebSocket connection with auto-reconnect"
```

### Task 6.4: FeishuChannel 组装

- [ ] **Step 1: 创建 `src/channel/feishu/mod.rs`**

实现 `FeishuChannel` 结构体，组装 api + ws：
- `new(config: &FeishuSection) -> Self`
- 实现 `Channel::start()`: 启动 `ws::run_with_reconnect`
- 实现 `Channel::send_message()`: 根据 `reply_to` 调用 `api.reply_text()` 或 `api.send_text()`
- 实现 `Channel::name() -> "feishu"`

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/channel/feishu/mod.rs
git commit -m "feat: assemble FeishuChannel implementing Channel trait"
```

---

## Phase 7: Gateway 消息路由

**目标：** 实现 Gateway，连通 Channel → Agent → Memory 完整链路。

**Files:**
- Create: `agent/src/gateway.rs`

### Task 7.1: 实现 Gateway

- [ ] **Step 1: 创建 `src/gateway.rs`**

实现 `Gateway`（参考设计规格书 §6）：
- 字段：channels, agent, memory, chat_locks (DashMap), recent_ids (LruCache)
- `new(channels, agent, memory) -> Self`: 初始化 LRU 容量 1000
- `run() -> Result<()>`:
  1. 创建 mpsc channel (256)
  2. spawn 所有 channel.start(tx)
  3. 主循环：recv → dedup → per-chat lock → spawn { load history → agent.handle → save_conversation → send_message }

- [ ] **Step 2: 在 `main.rs` 中 `mod gateway;`，运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/gateway.rs src/main.rs
git commit -m "feat: implement Gateway with message dedup and per-chat serialization"
```

---

## Phase 8: Heartbeat 定时任务

**目标：** 实现 Heartbeat，定时触发 agent 处理。

**Files:**
- Create: `agent/src/heartbeat.rs`

### Task 8.1: 实现 Heartbeat

- [ ] **Step 1: 创建 `src/heartbeat.rs`**

实现 `Heartbeat`（参考设计规格书 §10）：
- `new(config: &HeartbeatSection, agent, memory, channels, workspace_path) -> Self`
- `run() -> Result<()>`:
  1. `tokio::time::interval(Duration::from_secs(interval_minutes * 60))`
  2. 每次 tick: 读 HEARTBEAT.md → 构建 heartbeat InboundMessage → load history → agent.handle → save_conversation → 判断 HEARTBEAT_OK → 发送通知

- [ ] **Step 2: 运行 `cargo check`**

- [ ] **Step 3: Commit**

```bash
git add src/heartbeat.rs src/main.rs
git commit -m "feat: implement Heartbeat periodic task with HEARTBEAT_OK convention"
```

---

## Phase 9: 主入口 + Workspace + 优雅关机

**目标：** 组装所有模块，创建 workspace 文件，实现优雅关机。

**Files:**
- Modify: `agent/src/main.rs`
- Create: `agent/workspace/AGENTS.md`
- Create: `agent/workspace/SOUL.md`
- Create: `agent/workspace/TOOLS.md`
- Create: `agent/workspace/USER.md`
- Create: `agent/workspace/MEMORY.md`
- Create: `agent/workspace/HEARTBEAT.md`

### Task 9.1: 创建 workspace 模板文件

- [ ] **Step 1: 创建 6 个 workspace markdown 文件**

每个文件包含占位内容和注释说明（参考设计规格书 §12）：

`AGENTS.md`:
```markdown
# Agent 行为指令

你是用户的私人 AI 助理。

## 规则
- 收到用户消息后认真分析需求，选择合适的工具完成任务
- 如果不确定，先询问用户
- 保持回复简洁有用
```

类似地为其他 5 个文件创建基础模板。

- [ ] **Step 2: Commit**

```bash
git add workspace/
git commit -m "feat: add workspace template files (AGENTS, SOUL, TOOLS, USER, MEMORY, HEARTBEAT)"
```

### Task 9.2: 完成 main.rs 组装

- [ ] **Step 1: 重写 `src/main.rs`**

```rust
// 1. 解析命令行参数（clap）: --config 指定配置文件路径
// 2. 加载配置
// 3. 初始化 tracing
// 4. 初始化 MemoryStore
// 5. 创建 LLM client（工厂函数）
// 6. 创建 ToolRegistry（工厂函数）
// 7. 创建 AgentCore
// 8. 创建 FeishuChannel
// 9. 创建 Gateway
// 10. 启动 Gateway（tokio::spawn）
// 11. 如果 heartbeat.enabled，启动 Heartbeat（tokio::spawn）
// 12. 监听 SIGINT/SIGTERM（tokio::signal）
// 13. 收到信号后优雅关机
```

- [ ] **Step 2: 运行 `cargo build` 确认编译通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire up main entry point with all modules and graceful shutdown"
```

### Task 9.3: 优雅关机

参考设计规格书 §16，完整实现 5 步关机流程。

- [ ] **Step 1: 在 main.rs 实现 shutdown 逻辑**

使用 `tokio::signal::ctrl_c()` + `CancellationToken` 模式：
1. 收到 SIGINT/SIGTERM → log "Shutting down..."
2. 关闭 mpsc sender（停止接收新消息）
3. 等待正在处理的消息完成（`JoinSet` + 30 秒超时，超时则强制取消）
4. 显式关闭 WebSocket 连接（调用 `close` frame）
5. 关闭 SQLite 连接池（`pool.close().await` 确保 pending writes 刷入磁盘）
6. 退出

- [ ] **Step 2: 运行 `cargo build`**

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: implement graceful shutdown on SIGINT/SIGTERM"
```

---

## Phase 10: 集成测试 + 端到端验证

**目标：** 确保所有模块正确集成，端到端流程可用。

**Files:**
- Create: `agent/tests/integration_test.rs`

### Task 10.1: 集成测试 — Agent + Memory + Tools

- [ ] **Step 1: 创建 `tests/integration_test.rs`**

使用 mock LlmClient 测试完整链路：
- 创建 in-memory MemoryStore
- 注册所有工具（shell/file/memory_tool，web_fetch 可 skip）
- 创建 AgentCore
- 发送 InboundMessage，验证：
  - 纯文本回复正确
  - tool calling 循环正确执行
  - 对话历史正确保存到 SQLite

- [ ] **Step 2: 运行测试**

Run: `cargo test --test integration_test`
Expected: 通过

- [ ] **Step 3: Commit**

```bash
git add tests/integration_test.rs
git commit -m "test: add integration tests for Agent + Memory + Tools pipeline"
```

### Task 10.2: 编译验证 + 最终检查

- [ ] **Step 1: 完整编译**

Run: `cargo build --release`
Expected: 编译成功

- [ ] **Step 2: 运行所有测试**

Run: `cargo test`
Expected: 全部通过

- [ ] **Step 3: cargo clippy 检查**

Run: `cargo clippy -- -D warnings`
Expected: 无警告

- [ ] **Step 4: 最终 Commit**

```bash
git add -A
git commit -m "chore: final build verification - all tests pass, clippy clean"
```
