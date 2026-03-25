# ANQ Agent v1 — 设计规格书

## 1. 项目概述

用 Rust 实现一个轻量级私人 AI 助理，通过飞书 WebSocket 长连接收发消息，调用 LLM API 处理请求，支持 tool calling、持久记忆和定时任务。设计上预留扩展性，后续可接入 Telegram / Slack 等平台。

### 第一版（v1）范围

- 飞书 WebSocket 长连接收发消息
- 多 LLM 支持：Anthropic client + OpenAI-compatible client（覆盖 GPT/Gemini/DeepSeek/Qwen/MiMo 等）
- 多轮对话 + SQLite 持久化历史
- Tool calling agentic loop（6 个内置工具）
- 接收多种消息类型（Text/Image/File/RichText），发送纯文本
- Heartbeat 定时任务
- 全部可配置项通过 TOML 管理

---

## 2. 项目结构

单 crate 方案，模块间通过公共接口通信，预留未来拆分为 Cargo workspace 的能力。

```
agent/
├── Cargo.toml
├── config.toml                 # 配置文件
├── src/
│   ├── main.rs                 # 入口：加载配置 → 启动各模块
│   ├── config.rs               # AppConfig（TOML 反序列化）
│   ├── types.rs                # 公共类型：InboundMessage, OutboundMessage, ChatMessage
│   │
│   ├── channel/
│   │   ├── mod.rs              # trait Channel 定义 + ChannelMessage 枚举
│   │   └── feishu/
│   │       ├── mod.rs          # FeishuChannel 实现
│   │       ├── ws.rs           # WebSocket 连接、重连、心跳
│   │       ├── api.rs          # REST API（发消息、token 刷新）
│   │       └── types.rs        # 飞书事件 JSON 结构体
│   │
│   ├── gateway.rs              # Gateway：消息路由 + 会话管理
│   │
│   ├── agent/
│   │   ├── mod.rs              # AgentCore：agentic loop
│   │   ├── context.rs          # 上下文构建（system prompt + memory + history）
│   │   └── prompt.rs           # System prompt 模板
│   │
│   ├── llm/
│   │   ├── mod.rs              # trait LlmClient + LlmResponse 类型
│   │   ├── anthropic.rs        # Anthropic Claude API
│   │   └── openai_compat.rs    # OpenAI 兼容协议（覆盖 GPT/Gemini/DeepSeek/Qwen/MiMo）
│   │
│   ├── tool/
│   │   ├── mod.rs              # trait Tool + ToolRegistry
│   │   ├── shell.rs            # shell_exec
│   │   ├── web.rs              # web_fetch
│   │   ├── file.rs             # file_read + file_write
│   │   └── memory_tool.rs      # memory_save + memory_search
│   │
│   ├── memory/
│   │   ├── mod.rs              # MemoryStore（SQLite 操作）
│   │   └── schema.sql          # 建表语句
│   │
│   └── heartbeat.rs            # Heartbeat 定时任务
│
├── workspace/                   # Agent 工作区
│   ├── AGENTS.md               # 行为指令：角色定义、决策规则、约束
│   ├── SOUL.md                 # 性格设定：语气、风格、口头禅
│   ├── TOOLS.md                # 工具使用指南：环境信息、安全红线、扩展规范
│   ├── USER.md                 # 用户画像：称呼、偏好、时区
│   ├── MEMORY.md               # 预置记忆：启动时加载的重要事实
│   └── HEARTBEAT.md            # Heartbeat prompt：定时任务触发时的提示词
│
└── tests/
    ├── channel_test.rs
    ├── agent_test.rs
    └── tool_test.rs
```

### 模块边界规则（面向未来拆分）

> **拆分提醒**：当项目代码量超过 5000 行或需要接入第二个 channel 时，应考虑拆分为 Cargo workspace。拆分路径：
>
> - `types.rs` → `anq-core` crate（公共类型 + trait 定义）
> - `channel/` → `anq-feishu` crate（+ 未来 `anq-telegram` 等）
> - `llm/` → `anq-llm` crate
> - `tool/` → `anq-tools` crate
> - `memory/` → `anq-memory` crate
> - `main.rs` + `gateway.rs` + `agent/` + `heartbeat.rs` → `anq-agent` 主 crate
>
> 为确保低成本拆分，当前须遵守：
>
> 1. 每个顶级模块只通过 `mod.rs` 暴露公共接口
> 2. 跨模块通信**只依赖 `types.rs` 中的公共类型**和各模块 `mod.rs` 导出的 trait
> 3. 禁止跨模块访问子模块内部类型（如 `agent/` 不能 `use crate::channel::feishu::types::*`）

---

## 3. 公共类型（types.rs）

```rust
/// 入站消息（从 channel 到 gateway）
pub struct InboundMessage {
    pub channel: String,           // "feishu"
    pub chat_id: String,           // 会话 ID
    pub sender_id: String,         // 发送者 ID
    pub message_id: String,        // 消息 ID（用于回复）
    pub content: MessageContent,   // 消息内容
    pub timestamp: i64,
}

/// 消息内容（接收支持多种，发送只用 Text）
pub enum MessageContent {
    Text(String),
    Image { key: String },
    File { key: String, name: String },
    RichText(serde_json::Value),
}

/// 出站消息（从 gateway 到 channel）
pub struct OutboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub reply_to: Option<String>,
    pub content: String,           // v1 只发纯文本
}

/// LLM 对话消息
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,    // assistant 发起的 tool calls
    pub tool_call_id: Option<String>,         // tool 结果对应的 call id
}

pub enum Role { System, User, Assistant, Tool }

/// Tool calling 相关
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

/// Heartbeat 虚拟消息构造
impl InboundMessage {
    pub fn heartbeat(prompt: &str) -> Self {
        Self {
            channel: "__heartbeat__".into(),
            chat_id: "__heartbeat__".into(),
            sender_id: "__system__".into(),
            message_id: String::new(),
            content: MessageContent::Text(prompt.to_string()),
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}
```

> **关于 `ChatMessage.content` 与 Anthropic 多 content block 的处理**：
>
> `ChatMessage` 保持 `content: String` 的简单设计，不引入 `Vec<ContentBlock>` 泛化。
> Anthropic API 返回的 assistant 消息可能同时包含 text block 和 tool_use block：
>
> - `anthropic.rs` 内部负责将多 block 响应拆解为通用 `LlmResponse` 格式
> - text 部分提取到 `LlmResponse.text`
> - tool_use 部分映射到 `LlmResponse.tool_calls`
> - 存入 SQLite 时，`content` 存文本部分，`tool_calls` 列存 JSON 序列化的 tool calls
>
> 这样各 LLM client 内部处理格式差异，外部统一使用简单类型。

---

## 4. LLM 抽象层（llm/）

### trait 定义（mod.rs）

```rust
pub trait LlmClient: Send + Sync {
    /// 发送对话请求，返回文本或 tool calls
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse>;
}

/// LLM 可能同时返回文本和 tool calls（如 Claude 的 "Let me look that up" + tool_use）
pub struct LlmResponse {
    pub text: Option<String>,       // 文本回复（可能为空）
    pub tool_calls: Vec<ToolCall>,  // tool calls（为空表示无工具调用）
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema
}
```

> 注：Rust edition 2024 原生支持 async fn in trait，无需 `async-trait` crate。对于需要 `dyn LlmClient` 的场景，使用 `Box<dyn Future>` 手动装箱。

### anthropic.rs

- 请求：`POST https://api.anthropic.com/v1/messages`
- Claude 使用 `system` 参数（非 messages 数组），tool calling 格式为 `tool_use` / `tool_result` content block
- Header：`x-api-key` + `anthropic-version`
- 内部负责将通用 `ChatMessage` / `ToolDefinition` 转换为 Claude 特定格式

### openai_compat.rs

- 请求：`POST {base_url}/v1/chat/completions`
- 通过不同 `base_url` + `api_key` 覆盖所有 OpenAI 兼容 provider
- 消息格式直接映射，几乎 1:1

---

## 5. Channel 层（channel/）

### trait 定义（mod.rs）

```rust
pub trait Channel: Send + Sync + 'static {
    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()>;
    async fn send_message(&self, msg: OutboundMessage) -> Result<()>;
    fn name(&self) -> &str;
}
```

### 飞书实现（channel/feishu/）

**ws.rs — WebSocket 连接管理**：

```
启动
  → 获取 tenant_access_token
  → 获取 WS endpoint URL
  → 连接 WebSocket
  → 循环：
      收到消息事件 → 解析 → 白名单检查 → InboundMessage → tx.send()
      收到 ping → 回复 pong
      连接断开 → 指数退避重连（1s, 2s, 4s, 8s... 最大 60s）
```

**api.rs — REST API 封装**：

```rust
pub struct FeishuApi {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    token: RwLock<TokenState>,  // 缓存 token + 过期时间
}

impl FeishuApi {
    /// 获取/刷新 tenant_access_token（2小时有效，提前 5 分钟刷新）
    async fn ensure_token(&self) -> Result<String>;
    /// 发送文本消息
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<()>;
    /// 回复某条消息
    async fn reply_text(&self, message_id: &str, text: &str) -> Result<()>;
    /// 获取 WebSocket endpoint URL
    async fn get_ws_endpoint(&self) -> Result<String>;
}
```

**关键约束**：

- 收到事件后立即通过 tx 发给 Gateway，不做阻塞处理（应对飞书 3 秒超时）
- `allow_from` 白名单：ws.rs 解析事件后检查 sender_id，不在白名单直接丢弃
- Token 管理：仅保存在内存，提前 5 分钟刷新，401 时自动重试

**types.rs**：

- 定义 `im.message.receive_v1` 事件的反序列化结构
- 通过 `msg_type` 字段判断消息类型：`text` / `image` / `file` / `post`

---

## 6. Gateway（gateway.rs）

```rust
pub struct Gateway {
    channels: Vec<Arc<dyn Channel>>,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    /// 每个 chat_id 一把锁，保证同一会话的消息串行处理
    chat_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// 最近处理过的 message_id，用于飞书事件去重
    recent_ids: Mutex<LruCache<String, ()>>,  // 容量 1000
}

impl Gateway {
    pub async fn run(&self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<InboundMessage>(256);

        // 启动所有 channel
        for ch in &self.channels {
            let tx = tx.clone();
            let ch = ch.clone();
            tokio::spawn(async move { ch.start(tx).await });
        }

        // 主消息循环
        while let Some(msg) = rx.recv().await {
            // 消息去重：飞书可能推送重复事件
            if self.recent_ids.lock().await.put(msg.message_id.clone(), ()).is_some() {
                continue; // 已处理过，跳过
            }

            let agent = self.agent.clone();
            let memory = self.memory.clone();
            let channels = self.channels.clone();
            // 获取该 chat_id 的锁，保证同一会话串行处理
            let chat_lock = self.chat_locks
                .entry(msg.chat_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();

            tokio::spawn(async move {
                let _guard = chat_lock.lock().await;

                // 从 SQLite 加载历史
                let history = memory
                    .get_history(&msg.chat_id, 20)
                    .await
                    .unwrap_or_default();

                // agent 处理（返回 reply + 中间过程的所有 messages）
                let (reply, conversation) = agent.handle(&msg, &history).await;

                // 批量保存本轮对话到 SQLite（user msg + tool 中间过程 + assistant reply）
                memory.save_conversation(&msg.chat_id, &conversation).await.ok();

                // 发送回复
                if let Some(ch) = channels.iter().find(|c| c.name() == msg.channel) {
                    ch.send_message(reply).await.ok();
                }
            });
        }
        Ok(())
    }
}
```

**设计要点**：

- **无内存 Session**：历史全从 SQLite 加载。SQLite 本地查询 20 条 <1ms，无需内存缓存。重启不丢状态
- **per-chat 串行锁**：`DashMap<chat_id, Mutex>` 确保同一会话的消息按序处理，不同会话并发
- **消息去重**：`LruCache<message_id>` 防止飞书重复推送导致重复处理（容量 1000，覆盖短时间内的重复）
- **批量保存**：`save_conversation()` 一次性保存完整对话轮次（user + tool 中间过程 + assistant），而非逐条保存
- **history_limit**：从 `config.memory.history_limit` 读取

---

## 7. Agent Core — Agentic Loop（agent/）

```rust
pub struct AgentCore {
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
}

impl AgentCore {
    /// 返回 (最终回复, 本轮完整对话记录) — 对话记录用于持久化到 SQLite
    pub async fn handle(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
    ) -> (OutboundMessage, Vec<ChatMessage>) {
        // 1. 构建上下文
        let system_prompt = self.build_system_prompt();
        let memories = self.memory.search(&msg.text_content(), 5).await;
        let mut messages = Vec::new();
        messages.push(ChatMessage::system(&system_prompt));
        if !memories.is_empty() {
            messages.push(ChatMessage::system(&format_memories(&memories)));
        }
        messages.extend_from_slice(history);
        messages.push(ChatMessage::user(&msg.content.to_text()));

        // 2. 获取工具定义
        let tool_defs = self.tools.definitions();

        // 3. Agentic loop（最多 max_tool_rounds 轮）
        for _ in 0..self.config.agent.max_tool_rounds {
            let response = self.llm.chat(&messages, &tool_defs).await;

            match response {
                Ok(resp) => {
                    if resp.tool_calls.is_empty() {
                        // 无 tool calls → 最终回复，跳出循环
                        let text = resp.text.unwrap_or_default();
                        messages.push(ChatMessage::assistant(&text));
                        let reply = OutboundMessage {
                            channel: msg.channel.clone(),
                            chat_id: msg.chat_id.clone(),
                            reply_to: Some(msg.message_id.clone()),
                            content: text,
                        };
                        return (reply, messages);
                    }

                    // 有 tool calls → 记录 assistant 消息（可能含 text + tool_calls）
                    messages.push(ChatMessage::assistant_with_tools(
                        resp.text.as_deref(),
                        &resp.tool_calls,
                    ));

                    // 并发执行所有 tool calls
                    let results = self.tools.execute_batch(&resp.tool_calls).await;
                    for result in results {
                        messages.push(ChatMessage::tool_result(&result));
                    }
                    // 继续循环，让 LLM 看到 tool 结果
                }
                Err(e) => {
                    let reply = OutboundMessage::error(msg, &format!("LLM 调用失败: {e}"));
                    return (reply, messages);
                }
            }
        }

        let reply = OutboundMessage::error(msg, "处理超过最大轮次限制，已停止");
        (reply, messages)
    }
}
```

**context.rs — System prompt 拼装顺序**：

```
1. SOUL.md         → 性格基底
2. AGENTS.md       → 行为规则
3. TOOLS.md        → 工具指南 + 环境信息 + 红线
4. USER.md         → 用户信息
5. MEMORY.md       → 预置记忆
6. SQLite 记忆搜索  → 动态相关记忆
7. 对话历史
8. 当前用户消息
```

**关键设计**：

- 最大轮次从 `config.agent.max_tool_rounds` 读取（默认 10）
- 并发执行 tool calls（`join_all`），单个工具失败返回错误信息给 LLM，不终止 loop
- 非文本消息：`to_text()` 对 Image/File 返回描述文本（如 `[图片: image_key_xxx]`）
- System prompt 文件（SOUL.md 等）不存在则跳过，不报错

---

## 8. Tool Registry + 6 个内置工具（tool/）

### trait 定义（mod.rs）

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn definitions(&self) -> Vec<ToolDefinition>;
    pub async fn execute_batch(&self, calls: &[ToolCall]) -> Vec<ToolResult>;
}
```

### 内置工具

| 工具 | 文件 | 输入 | 输出 | 安全约束 |
|------|------|------|------|----------|
| `shell_exec` | shell.rs | `command: String` | stdout + stderr | 白名单命令，超时 `shell_timeout_secs` |
| `web_fetch` | web.rs | `url: String` | 网页纯文本 | 超时 `web_fetch_timeout_secs`，最大 `web_fetch_max_bytes` |
| `file_read` | file.rs | `path: String` | 文件内容 | 限制在 `file_access_dir` 内 |
| `file_write` | file.rs | `path: String, content: String` | 写入确认 | 限制在 `file_access_dir` 内 |
| `memory_save` | memory_tool.rs | `key: String, content: String, tags: Vec<String>` | 保存确认 | 无 |
| `memory_search` | memory_tool.rs | `query: String, limit?: usize` | 匹配记忆列表 | 默认 `memory_tool_search_limit` |

**实现要点**：

- **shell_exec**：`tokio::process::Command::new(cmd).args(parsed_args)` 直接执行，**不经过 shell**（不使用 `sh -c`），不支持管道/重定向等 shell 元字符。校验命令名在白名单内，超时 kill
- **web_fetch**：`reqwest::get(url)`，最多跟随 5 次重定向，截断超长内容，strip HTML 标签返回纯文本。JSON 响应直接返回原文。二进制内容返回错误提示
- **file_read / file_write**：`tokio::fs`，路径 canonicalize 后校验前缀，防止路径穿越
- **memory_save / memory_search**：调用 `MemoryStore` 方法的薄封装
- **工具开关**：配置中 `xxx_enabled = false` 时不注册到 ToolRegistry

---

## 9. Memory Store（memory/）

### schema.sql

```sql
CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id     TEXT NOT NULL,
    role        TEXT NOT NULL,
    content     TEXT NOT NULL,
    tool_calls  TEXT,
    tool_call_id TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_chat_time ON messages(chat_id, created_at);

CREATE VIRTUAL TABLE IF NOT EXISTS memories USING fts5(
    key, content, tags, created_at UNINDEXED
);
```

### MemoryStore（mod.rs）

```rust
pub struct MemoryStore {
    pool: SqlitePool,
}

impl MemoryStore {
    pub async fn new(db_path: &str) -> Result<Self>;

    // 对话历史
    /// 批量保存一轮完整对话（user msg + tool 中间过程 + assistant reply）
    pub async fn save_conversation(&self, chat_id: &str, messages: &[ChatMessage]) -> Result<()>;
    pub async fn get_history(&self, chat_id: &str, limit: usize) -> Result<Vec<ChatMessage>>;

    // 长期记忆
    pub async fn save_memory(&self, key: &str, content: &str, tags: &[String]) -> Result<()>;
    pub async fn search_memory(&self, query: &str, limit: usize) -> Result<Vec<Memory>>;
}
```

**设计要点**：

- Tool 中间过程（tool_call + tool_result）也存入 messages 表，加载后 LLM 可见完整推理链
- FTS5 使用 SQLite 内置 unicode61 tokenizer，对中文按字切分
- `MemoryStore::new()` 自动建表，db 文件不存在则创建
- limit 参数全从配置读取

---

## 10. Heartbeat（heartbeat.rs）

```rust
pub struct Heartbeat {
    interval: Duration,
    agent: Arc<AgentCore>,
    memory: Arc<MemoryStore>,
    channels: Vec<Arc<dyn Channel>>,
    prompt_path: PathBuf,          // workspace/HEARTBEAT.md
    notify_chat_id: String,
    notify_channel: String,
}

impl Heartbeat {
    pub async fn run(&self) -> Result<()> {
        let mut interval = tokio::time::interval(self.interval);
        loop {
            interval.tick().await;
            let prompt = tokio::fs::read_to_string(&self.prompt_path).await.unwrap_or_default();
            if prompt.is_empty() { continue; }

            let msg = InboundMessage::heartbeat(&prompt);
            let history = self.memory.get_history(&msg.chat_id, 5).await.unwrap_or_default();
            let (reply, conversation) = self.agent.handle(&msg, &history).await;

            // 保存 heartbeat 对话历史（独立 chat_id "__heartbeat__"）
            self.memory.save_conversation(&msg.chat_id, &conversation).await.ok();

            // LLM 回复包含 "HEARTBEAT_OK" 则不通知用户
            if reply.content.contains("HEARTBEAT_OK") { continue; }

            if let Some(ch) = self.channels.iter().find(|c| c.name() == self.notify_channel) {
                ch.send_message(reply).await.ok();
            }
        }
    }
}
```

**设计要点**：

- 每次 tick 重新读 HEARTBEAT.md，修改 prompt 不用重启
- Heartbeat 对话历史独立存储，和用户对话不混淆
- 完整走 agent loop，可触发 tool calling

---

## 11. 配置文件（config.toml）

```toml
[app]
name = "anq-agent"
workspace = "./workspace"
log_level = "info"                     # trace | debug | info | warn | error

[feishu]
app_id = "cli_xxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxx"
allow_from = ["ou_xxxxx"]

[llm]
provider = "anthropic"                 # "anthropic" | "openai_compat"
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-xxxxxxxx"
base_url = ""                          # anthropic 留空；openai_compat 必填
max_tokens = 4096
temperature = 0.7

[memory]
db_path = "./data/memory.db"
history_limit = 20
search_limit = 5

[heartbeat]
enabled = false
interval_minutes = 30
notify_channel = "feishu"
notify_chat_id = "oc_xxxxx"

[tools]
shell_enabled = true
shell_allowed_commands = ["ls", "cat", "grep", "find", "date", "curl"]
shell_timeout_secs = 30

web_fetch_enabled = true
web_fetch_timeout_secs = 10
web_fetch_max_bytes = 102400

file_enabled = true
file_access_dir = "./workspace"

memory_tool_enabled = true
memory_tool_search_limit = 5

[agent]
max_tool_rounds = 10
system_prompt_file = ""                # 自定义 system prompt，空则从 workspace/*.md 拼装
```

**config.rs**：每个 section 独立结构体，所有可选字段用 `#[serde(default)]` 提供默认值。必填字段：`feishu.app_id`、`feishu.app_secret`、`llm.api_key`。敏感字段用 `secrecy::SecretString` 包装。

---

## 12. Workspace 工作区

```
workspace/
├── AGENTS.md      # 行为指令：角色定义、决策规则、约束、启动指引
├── SOUL.md        # 性格设定：语气、风格、口头禅
├── TOOLS.md       # 工具使用指南：本地环境配置、安全红线、扩展技能包规范
├── USER.md        # 用户画像：称呼、偏好、时区
├── MEMORY.md      # 预置记忆：启动时加载的重要事实
└── HEARTBEAT.md   # Heartbeat prompt：定时任务触发时的提示词
```

| 文件 | 定位 | 加载时机 |
|------|------|----------|
| SOUL.md | 性格、语气、风格 | 每次构建 system prompt |
| AGENTS.md | 行为逻辑、决策规则 | 每次构建 system prompt |
| TOOLS.md | 工具使用指南、环境信息、安全红线 | 每次构建 system prompt |
| USER.md | 用户个人信息和偏好 | 每次构建 system prompt |
| MEMORY.md | 预置的重要事实 | 每次构建 system prompt |
| HEARTBEAT.md | 定时任务 prompt | 每次 heartbeat tick |

---

## 13. 依赖库

```toml
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
```

> 注：不使用 `async-trait` crate。Rust edition 2024 原生支持 async fn in trait。对于需要 `dyn Trait` 的场景，手动返回 `Pin<Box<dyn Future>>` 即可。

---

## 14. 安全考虑

- **白名单**：`allow_from` 限制可交互的用户
- **Tool 沙箱**：shell_exec 命令白名单 + 超时 kill
- **文件访问**：canonicalize 路径，校验前缀在 `file_access_dir` 内，防穿越
- **API Key**：`secrecy::SecretString` 包装，防日志泄露
- **Token**：tenant_access_token 仅内存存储，不落盘
- **3 秒超时**：收到飞书事件后立即转发给 Gateway，不阻塞
- **消息去重**：Gateway 维护最近 1000 条 message_id 的 LRU 缓存，防止飞书重复推送
- **配置文件明文秘钥**：支持环境变量引用，如 `api_key = "${ANQ_LLM_API_KEY}"`，优先读环境变量

---

## 15. LLM 调用重试策略

对 LLM API 的调用采用指数退避重试：

- **可重试状态码**：429（Rate Limit）、500（Server Error）、529（Overload，Anthropic 特有）
- **重试次数**：最多 3 次
- **退避策略**：1s → 2s → 4s（指数退避）
- **429 特殊处理**：如果响应含 `retry-after` header，使用该值作为等待时间
- **不可重试**：400、401、403 等客户端错误，立即返回错误

---

## 16. 优雅关机

- 监听 `SIGTERM` / `SIGINT`（`tokio::signal`）
- 收到信号后：
  1. 停止接收新消息（关闭 mpsc sender）
  2. 等待正在处理的消息完成（设超时 30s）
  3. 关闭 WebSocket 连接
  4. 刷新 SQLite pending writes
  5. 退出

---

## 17. 已知限制（v1）

- **FTS5 中文搜索质量**：SQLite unicode61 tokenizer 按字切分中文，搜索 "天气" 会匹配含 "天" 或 "气" 的所有记录。v2 可考虑接入 jieba 分词或 `LIKE '%query%'` 混合方案
- **文件工具可访问 workspace**：`file_access_dir` 默认指向 workspace，LLM 理论上可通过 `file_write` 修改 SOUL.md 等 prompt 文件。如需限制，可配置独立的数据目录
- **无消息流式输出**：agentic loop 可能耗时较长，用户在此期间无进度提示。v2 可考虑发送 "思考中..." 中间消息

---

## 18. 关键流程

### 消息处理

```
飞书用户发消息
  → FeishuChannel (WS) 收到事件
  → 白名单检查
  → InboundMessage → mpsc → Gateway
  → Gateway: SQLite 加载历史 → AgentCore.handle()
  → AgentCore: 构建上下文 → LLM → [tool loop] → 最终回复
  → Gateway: 保存对话到 SQLite → channel.send_message()
  → FeishuChannel: REST API 发送回复
```

### 飞书 WebSocket 连接

```
启动 → 获取 token → 获取 WS endpoint → 连接
  → 消息循环（收消息/ping-pong）
  → 断连 → 指数退避重连（1s → 2s → 4s → ... → 60s max）
```
