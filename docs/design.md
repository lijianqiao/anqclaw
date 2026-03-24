# Rust 私人助理 Agent — 架构设计 v1

## 1. 项目概述

用 Rust 实现一个类 OpenClaw 的私人 AI 助理，通过飞书长连接（WebSocket）收发消息，
调用 LLM API 处理请求，支持 tool calling、持久记忆和定时任务。
设计上预留扩展性，后续可接入 Telegram / Slack 等平台。

---

## 2. 核心架构

```
┌─────────────────────────────────────────────────┐
│                Agent Runtime (tokio)             │
│                                                  │
│  ┌─────────────────────────────────────────────┐ │
│  │         Channel Layer (trait Channel)        │ │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐    │ │
│  │  │ Feishu   │ │ Telegram │ │  Slack   │    │ │
│  │  │   WS     │ │ (future) │ │ (future) │    │ │
│  │  └────┬─────┘ └──────────┘ └──────────┘    │ │
│  └───────┼─────────────────────────────────────┘ │
│          ▼                                       │
│  ┌─────────────────────────────────────────────┐ │
│  │     Gateway / Router (消息路由 + 会话管理)    │ │
│  └──────────────────┬──────────────────────────┘ │
│                     ▼                            │
│  ┌─────────────────────────────────────────────┐ │
│  │              Agent Core                      │ │
│  │   msg → LLM → tool_call → LLM → reply      │ │
│  └───┬──────────────┬──────────────┬───────────┘ │
│      ▼              ▼              ▼             │
│ ┌──────────┐  ┌──────────┐  ┌──────────┐       │
│ │  Tools   │  │  Memory  │  │Heartbeat │       │
│ │ Registry │  │  Store   │  │  Cron    │       │
│ └──────────┘  └──────────┘  └──────────┘       │
│                                                  │
│  ┌─────────────────────────────────────────────┐ │
│  │         Config (TOML) + Tracing Logger       │ │
│  └─────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────┘
         │                          │
    LLM API (HTTP)          Feishu REST API (HTTP)
  (Claude / GPT / ...)     (发消息 / 上传文件 / ...)
```

---

## 3. 模块设计

### 3.1 Channel Layer

**职责**: 抽象消息平台的收发能力。

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// 启动 channel，通过 tx 将收到的消息发到 Gateway
    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> Result<()>;

    /// 发送回复消息
    async fn send_message(&self, msg: OutboundMessage) -> Result<()>;

    /// channel 名称标识
    fn name(&self) -> &str;
}
```

**核心类型**:

```rust
pub struct InboundMessage {
    pub channel: String,         // "feishu"
    pub chat_id: String,         // 会话 ID
    pub sender_id: String,       // 发送者 ID
    pub message_id: String,      // 消息 ID（用于回复）
    pub content: MessageContent, // 文本/图片/文件
    pub timestamp: i64,
}

pub enum MessageContent {
    Text(String),
    Image { key: String },       // 飞书 image_key
    File { key: String, name: String },
    RichText(serde_json::Value), // 富文本 JSON
}

pub struct OutboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub reply_to: Option<String>, // 回复某条消息
    pub content: MessageContent,
}
```

### 3.2 Feishu Channel（首个实现）

**连接流程**:

```
1. POST /auth/v3/tenant_access_token/internal
   → 获取 tenant_access_token

2. POST /callback/ws/endpoint
   (Header: Authorization: Bearer {token})
   → 获取 WebSocket URL + ticket

3. 连接 WebSocket URL
   → 建连后平台推送事件（明文 JSON）

4. 收到消息事件后:
   - 解析事件 JSON（im.message.receive_v1）
   - 提取 chat_id, sender_id, content
   - 封装为 InboundMessage 发到 Gateway

5. 发送回复:
   - POST /im/v1/messages (REST API)
   - POST /im/v1/messages/{message_id}/reply

6. 心跳保活:
   - 定期 ping/pong 维持 WebSocket 连接
   - 断连自动重连（指数退避）
```

**关键约束**:

- 收到事件后 3 秒内需处理完（否则飞书会重推）
- 方案：先 ACK（快速回复"思考中..."卡片），再异步处理
- 每个应用最多 50 个 WebSocket 连接
- 消息推送为集群模式（多客户端只有一个收到）

**Token 管理**:

- tenant_access_token 有效期 2 小时
- 需要后台定时刷新，或在 401 时自动刷新

### 3.3 Gateway / Router

**职责**: 接收所有 channel 的消息，分发给 Agent 处理，管理会话。

```rust
pub struct Gateway {
    channels: Vec<Arc<dyn Channel>>,
    agent: Arc<AgentCore>,
    session_store: Arc<SessionStore>,
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
            let agent = self.agent.clone();
            let sessions = self.session_store.clone();
            tokio::spawn(async move {
                let session = sessions.get_or_create(&msg.chat_id).await;
                agent.handle(msg, session).await;
            });
        }
        Ok(())
    }
}
```

**Session 管理**:

```rust
pub struct Session {
    pub chat_id: String,
    pub history: Vec<ChatMessage>,  // 近 N 轮对话
    pub metadata: SessionMeta,      // 用户偏好等
}
```

### 3.4 Agent Core

**职责**: LLM 调用循环 — 核心 agentic loop。

```rust
pub struct AgentCore {
    llm_client: Arc<dyn LlmClient>,
    tool_registry: Arc<ToolRegistry>,
    memory: Arc<MemoryStore>,
    config: Arc<AppConfig>,
}

impl AgentCore {
    pub async fn handle(
        &self,
        msg: InboundMessage,
        session: &mut Session,
    ) -> Result<OutboundMessage> {
        // 1. 构建上下文：system prompt + memory + history + user msg
        let context = self.build_context(&msg, session).await?;

        // 2. Agentic loop
        let mut messages = context;
        loop {
            let response = self.llm_client.chat(&messages).await?;

            match response {
                LlmResponse::Text(text) => {
                    // 最终回复，跳出循环
                    session.push_assistant(&text);
                    return Ok(OutboundMessage::text(&msg, text));
                }
                LlmResponse::ToolCalls(calls) => {
                    // 执行所有 tool calls
                    let results = self.execute_tools(&calls).await?;
                    // 把 tool results 追加到 messages 继续循环
                    messages.push_tool_results(results);
                }
            }
        }
    }
}
```

**LLM Client 抽象**:

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<LlmResponse>;
}

pub enum LlmResponse {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}
```

先实现 `AnthropicClient`（Claude API），后续可加 `OpenAIClient` 等。

### 3.5 Tool Registry

**职责**: 管理和执行工具/技能。

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value; // JSON Schema
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}
```

**内置工具（v1）**:

| 工具名          | 功能               | 实现方式                  |
| --------------- | ------------------ | ------------------------- |
| `shell_exec`    | 执行 shell 命令    | `tokio::process::Command` |
| `web_fetch`     | 抓取网页内容       | `reqwest` GET             |
| `file_read`     | 读取本地文件       | `tokio::fs`               |
| `file_write`    | 写入本地文件       | `tokio::fs`               |
| `memory_save`   | 保存信息到长期记忆 | SQLite INSERT             |
| `memory_search` | 搜索长期记忆       | SQLite FTS5               |

### 3.6 Memory Store

**职责**: 持久化对话历史和长期记忆。

```rust
pub struct MemoryStore {
    db: SqlitePool, // sqlx
}

impl MemoryStore {
    /// 保存一条记忆
    pub async fn save(&self, key: &str, content: &str, tags: &[&str]) -> Result<()>;

    /// 搜索相关记忆（FTS5 全文检索）
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Memory>>;

    /// 获取最近 N 轮对话
    pub async fn get_history(&self, chat_id: &str, limit: usize) -> Result<Vec<ChatMessage>>;

    /// 保存对话消息
    pub async fn save_message(&self, chat_id: &str, msg: &ChatMessage) -> Result<()>;
}
```

**数据库 schema**:

```sql
-- 对话历史
CREATE TABLE messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id     TEXT NOT NULL,
    role        TEXT NOT NULL, -- user / assistant / tool
    content     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    INDEX idx_chat_time (chat_id, created_at)
);

-- 长期记忆（支持全文检索）
CREATE VIRTUAL TABLE memories USING fts5(
    key, content, tags, created_at UNINDEXED
);
```

### 3.7 Heartbeat (定时任务)

**职责**: 定期主动触发 agent，实现 proactive 能力。

```rust
pub struct Heartbeat {
    interval: Duration,   // 默认 30 分钟
    agent: Arc<AgentCore>,
    config: HeartbeatConfig,
}

impl Heartbeat {
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.interval);
        loop {
            interval.tick().await;
            // 构造 heartbeat prompt
            let prompt = self.config.prompt.clone();
            // 调用 agent 处理
            let result = self.agent.handle_heartbeat(&prompt).await;
            // 如果有需要通知用户的内容，通过 channel 发出
            if let Ok(Some(notification)) = result {
                self.send_notification(notification).await;
            }
        }
    }
}
```

---

## 4. 依赖库选型

| 功能        | crate                            | 版本         | 说明                                                       |
| ----------- | -------------------------------- | ------------ | ---------------------------------------------------------- |
| 异步运行时  | `tokio`                          | 1.x (latest) | 全功能：rt-multi-thread, macros, fs, process, time, signal |
| WebSocket   | `tokio-tungstenite`              | 0.29         | 飞书长连接客户端，启用 `rustls-tls-native-roots`           |
| HTTP 客户端 | `reqwest`                        | 0.12         | 飞书 REST API + LLM API 调用，启用 `json`, `rustls-tls`    |
| 序列化      | `serde` + `serde_json`           | 1.x          | JSON 处理                                                  |
| 数据库      | `sqlx`                           | 0.8          | SQLite 异步驱动，启用 `sqlite`, `runtime-tokio`            |
| 配置        | `toml` + `serde`                 | -            | TOML 配置文件解析                                          |
| 日志        | `tracing` + `tracing-subscriber` | 0.1 / 0.3    | 结构化日志                                                 |
| 错误处理    | `anyhow` + `thiserror`           | 1.x          | anyhow 给应用层，thiserror 给库层                          |
| 异步 trait  | `async-trait`                    | 0.1          | trait 中的 async fn（等 Rust async trait 稳定后可去掉）    |
| 命令行      | `clap`                           | 4.x          | CLI 参数解析                                               |
| 密钥管理    | `secrecy`                        | 0.10         | 防止 API key 意外打印到日志                                |

**Cargo.toml 核心依赖**:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = { version = "0.29", features = ["rustls-tls-native-roots"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
anyhow = "1"
thiserror = "2"
async-trait = "0.1"
clap = { version = "4", features = ["derive"] }
secrecy = "0.10"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
```

---

## 5. 项目结构

```
crab-agent/
├── Cargo.toml
├── config.toml               # 配置文件
├── src/
│   ├── main.rs                # 入口，启动 Gateway + Heartbeat
│   ├── config.rs              # 配置结构体
│   ├── types.rs               # InboundMessage, OutboundMessage 等公共类型
│   │
│   ├── channel/
│   │   ├── mod.rs             # trait Channel 定义
│   │   └── feishu/
│   │       ├── mod.rs         # FeishuChannel 实现
│   │       ├── ws.rs          # WebSocket 连接管理 + 重连
│   │       ├── api.rs         # 飞书 REST API 封装（发消息、token 刷新）
│   │       └── types.rs       # 飞书事件 JSON 类型定义
│   │
│   ├── gateway.rs             # Gateway + 会话管理
│   │
│   ├── agent/
│   │   ├── mod.rs             # AgentCore 实现
│   │   ├── context.rs         # 上下文构建（system prompt + memory + history）
│   │   └── prompt.rs          # System prompt 模板
│   │
│   ├── llm/
│   │   ├── mod.rs             # trait LlmClient
│   │   ├── anthropic.rs       # Claude API 实现
│   │   └── openai.rs          # OpenAI API 实现（可选）
│   │
│   ├── tool/
│   │   ├── mod.rs             # trait Tool + ToolRegistry
│   │   ├── shell.rs           # shell_exec
│   │   ├── web.rs             # web_fetch
│   │   ├── file.rs            # file_read / file_write
│   │   └── memory_tool.rs     # memory_save / memory_search
│   │
│   ├── memory/
│   │   ├── mod.rs             # MemoryStore
│   │   └── schema.sql         # SQLite 建表语句
│   │
│   └── heartbeat.rs           # Heartbeat 定时任务
│
├── workspace/                 # Agent 工作目录
│   ├── AGENTS.md              # Agent 人设 / 指令
│   ├── SOUL.md                # 性格设定
│   └── MEMORY.md              # 可选：启动时加载的记忆
│
└── tests/
    ├── channel_test.rs
    ├── agent_test.rs
    └── tool_test.rs
```

---

## 6. 配置文件 (config.toml)

```toml
[app]
name = "crab-agent"
workspace = "./workspace"
log_level = "info"          # trace, debug, info, warn, error

[feishu]
app_id = "cli_xxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxx"
# 允许接收消息的用户 ID 列表（安全白名单）
allow_from = ["ou_xxxxx"]

[llm]
provider = "anthropic"      # anthropic | openai
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-xxxxxxxx"
max_tokens = 4096
temperature = 0.7

[memory]
db_path = "./data/memory.db"
history_limit = 20          # 保留最近 N 轮对话作为上下文

[heartbeat]
enabled = false
interval_minutes = 30
prompt = "检查是否有需要我关注的事项。如果没有，回复 HEARTBEAT_OK。"

[tools]
shell_enabled = true
shell_allowed_commands = ["ls", "cat", "grep", "find", "date", "curl"]
web_fetch_enabled = true
file_access_dir = "./workspace"  # 限制文件操作范围
```

---

## 7. 关键流程

### 7.1 消息处理流程

```
用户在飞书发消息
    ↓
FeishuChannel (WebSocket) 收到 im.message.receive_v1 事件
    ↓
解析事件 → InboundMessage → 通过 mpsc::channel 发到 Gateway
    ↓
Gateway 查找/创建 Session → 调用 AgentCore.handle()
    ↓
AgentCore:
  1. 从 MemoryStore 加载 history + 相关记忆
  2. 拼装 messages: [system_prompt, memories, history, user_msg]
  3. 调用 LlmClient.chat()
  4. 如果返回 ToolCalls → 执行工具 → 结果追回 messages → 回到 3
  5. 如果返回 Text → 保存到 history → 封装 OutboundMessage
    ↓
Gateway 调用 FeishuChannel.send_message()
    ↓
FeishuChannel 通过 REST API POST 发送回复
```

### 7.2 飞书 WebSocket 连接管理

```
启动
  ↓
获取 tenant_access_token (POST /auth/v3/tenant_access_token/internal)
  ↓
获取 WS endpoint (POST /callback/ws/endpoint)
  ↓
连接 WebSocket → 认证成功
  ↓
┌──────── 消息接收循环 ←──────┐
│  收到消息 → 解析 → 分发      │
│  收到 ping → 回复 pong      │
│  连接断开 → 指数退避重连 ─────┘
```

---

## 8. 安全考虑

- **白名单机制**: `allow_from` 限制哪些用户可以与 bot 交互
- **Tool 沙箱**: shell_exec 限制可执行命令白名单
- **文件访问**: 限制在 `file_access_dir` 范围内
- **API Key**: 使用 `secrecy` crate，防止日志泄露
- **Token 存储**: tenant_access_token 仅保存在内存，不落盘
- **3 秒超时**: 收到事件后先回复"思考中"卡片，避免飞书重推

---

## 9. 扩展路径

| 阶段 | 内容                                     |
| ---- | ---------------------------------------- |
| v0.1 | 飞书 WS 收发 + Claude API 单轮对话       |
| v0.2 | 多轮对话 + 对话历史持久化                |
| v0.3 | Tool calling 循环 + 内置工具             |
| v0.4 | Memory 长期记忆 + FTS5 搜索              |
| v0.5 | Heartbeat 定时任务                       |
| v0.6 | 飞书消息卡片（富文本回复）               |
| v1.0 | 新增 Telegram/Slack channel              |
| v1.x | 自定义 Skill 插件系统（动态加载 WASM？） |