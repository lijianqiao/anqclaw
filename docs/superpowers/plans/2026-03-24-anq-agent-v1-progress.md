# ANQ Agent v1 — 实施进度追踪

> 每个阶段完成后更新本文档，标记完成状态和完成时间。

**实施计划:** `docs/superpowers/plans/2026-03-24-anq-agent-v1-plan.md`
**设计规格书:** `docs/superpowers/specs/2026-03-24-anq-agent-v1-design.md`

---

## 总览

| 阶段 | 内容 | 状态 | 完成时间 | 备注 |
|------|------|------|----------|------|
| Phase 1 | 项目脚手架 + 配置 + 公共类型 | - [x] 已完成 | 2026-03-24 | reqwest 0.13 feature `rustls-tls` → `rustls` |
| Phase 2 | Memory Store（SQLite） | - [x] 已完成 | 2026-03-24 | trigram tokenizer 支持中文子串搜索 |
| Phase 3 | LLM 抽象层 + 两个 Client | - [x] 已完成 | 2026-03-24 | LlmClient trait + Anthropic + OpenAI-compat |
| Phase 4 | Tool Registry + 6 个内置工具 | - [x] 已完成 | 2026-03-24 | 6 工具 + 路径沙箱 + shell 白名单 + 并发执行 |
| Phase 5 | Agent Core — Agentic Loop | - [ ] 未开始 | | |
| Phase 6 | 飞书 Channel 实现 | - [ ] 未开始 | | |
| Phase 7 | Gateway 消息路由 | - [ ] 未开始 | | |
| Phase 8 | Heartbeat 定时任务 | - [ ] 未开始 | | |
| Phase 9 | 主入口 + Workspace + 优雅关机 | - [ ] 未开始 | | |
| Phase 10 | 集成测试 + 端到端验证 | - [ ] 未开始 | | |

---

## Phase 1: 项目脚手架 + 配置 + 公共类型

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 1.1 | 更新 Cargo.toml 依赖 | - [x] 已完成 | reqwest feature `rustls-tls` → `rustls`（v0.13 改名） |
| 1.2 | 创建公共类型 types.rs | - [x] 已完成 | 9 个类型 + 便捷构造方法 |
| 1.3 | 创建配置模块 config.rs | - [x] 已完成 | 两阶段反序列化 + SecretString + ${ENV_VAR} |
| 1.4 | 初始化 tracing 日志 | - [x] 已完成 | JSON 格式输出到 stderr |

**阶段完成标志:** `cargo run` 能加载配置并输出日志

---

## Phase 2: Memory Store（SQLite）

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 2.1 | 创建 schema.sql | - [x] 已完成 | WAL 模式 + trigram FTS5 虚拟表 |
| 2.2 | 实现 MemoryStore | - [x] 已完成 | 6 个单元测试全部通过，含 tool_calls 轮次持久化 |

**阶段完成标志:** 6 个单元测试通过（history CRUD、tool_calls 序列化、FTS5 搜索、upsert、空查询）

---

## Phase 3: LLM 抽象层 + 两个 Client

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 3.1 | 定义 LlmClient trait | - [x] 已完成 | object-safe `Pin<Box<dyn Future>>` |
| 3.2 | 实现 OpenAI-compatible Client | - [x] 已完成 | 429/5xx 指数退避重试，覆盖 DeepSeek/Qwen/MiMo/Gemini |
| 3.3 | 实现 Anthropic Client | - [x] 已完成 | system 提取、tool_use/tool_result content block 映射、529 重试 |
| 3.4 | LLM Client 工厂函数 | - [x] 已完成 | `create_llm_client()` 按 provider 分发 |

**阶段完成标志:** `cargo check` 通过，两个 client 实现完整

---

## Phase 4: Tool Registry + 6 个内置工具

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 4.1 | Tool trait + ToolRegistry | - [x] 已完成 | object-safe trait, `execute_batch` 并发执行 + 错误隔离 |
| 4.2 | shell_exec 工具 | - [x] 已完成 | 白名单校验 + 超时 kill + 跨平台 (cmd/sh) |
| 4.3 | web_fetch 工具 | - [x] 已完成 | HTML strip + body 截断 + 空白折叠 |
| 4.4 | file_read + file_write 工具 | - [x] 已完成 | 路径 canonicalize 沙箱防护 + 自动创建父目录 |
| 4.5 | memory_save + memory_search 工具 | - [x] 已完成 | MemoryStore 薄封装，tags 可选 |
| 4.6 | 工具注册工厂 | - [x] 已完成 | 按 config 开关注册，带 tracing 日志 |

**阶段完成标志:** 工具单元测试通过（shell 白名单、file 路径穿越防护、memory 读写）

---

## Phase 5: Agent Core — Agentic Loop

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 5.1 | System prompt 构建 | - [ ] 未完成 | |
| 5.2 | AgentCore agentic loop | - [ ] 未完成 | |

**阶段完成标志:** 3 个单元测试通过（纯文本、tool loop、max rounds）

---

## Phase 6: 飞书 Channel 实现

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 6.1 | Channel trait + 飞书事件类型 | - [ ] 未完成 | |
| 6.2 | 飞书 REST API 封装 | - [ ] 未完成 | |
| 6.3 | 飞书 WebSocket 连接管理 | - [ ] 未完成 | |
| 6.4 | FeishuChannel 组装 | - [ ] 未完成 | |

**阶段完成标志:** `cargo check` 通过，Channel trait 实现完整

---

## Phase 7: Gateway 消息路由

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 7.1 | 实现 Gateway | - [ ] 未完成 | |

**阶段完成标志:** `cargo check` 通过，Gateway 能串联 Channel → Agent → Memory

---

## Phase 8: Heartbeat 定时任务

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 8.1 | 实现 Heartbeat | - [ ] 未完成 | |

**阶段完成标志:** `cargo check` 通过

---

## Phase 9: 主入口 + Workspace + 优雅关机

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 9.1 | 创建 workspace 模板文件 | - [ ] 未完成 | |
| 9.2 | 完成 main.rs 组装 | - [ ] 未完成 | |
| 9.3 | 优雅关机 | - [ ] 未完成 | |

**阶段完成标志:** `cargo build` 成功，程序能启动并响应 Ctrl+C

---

## Phase 10: 集成测试 + 端到端验证

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 10.1 | 集成测试 | - [ ] 未完成 | |
| 10.2 | 编译验证 + 最终检查 | - [ ] 未完成 | |

**阶段完成标志:** `cargo test` 全部通过，`cargo clippy` 无警告，`cargo build --release` 成功

---

## 变更记录

| 日期 | 变更内容 |
|------|----------|
| 2026-03-24 | 创建进度追踪文档 |
| 2026-03-24 | Phase 1 完成：脚手架 + 配置 + 公共类型 + tracing |
| 2026-03-24 | Phase 2 完成：SQLite MemoryStore，WAL + trigram FTS5，6 个测试全通过 |
| 2026-03-24 | Phase 3 完成：LlmClient trait + Anthropic/OpenAI-compat 双端实现 |
| 2026-03-24 | Phase 4 完成：Tool trait + ToolRegistry + 6 个内置工具（shell/web/file/memory） |
