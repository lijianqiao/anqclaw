# ANQ Agent v1 — 实施进度追踪

> 每个阶段完成后更新本文档，标记完成状态和完成时间。

**实施计划:** `docs/superpowers/plans/2026-03-24-anq-agent-v1-plan.md`
**设计规格书:** `docs/superpowers/specs/2026-03-24-anq-agent-v1-design.md`

---

## 总览

| 阶段 | 内容 | 状态 | 完成时间 | 备注 |
|------|------|------|----------|------|
| Phase 1 | 项目脚手架 + 配置 + 公共类型 | - [ ] 未开始 | | |
| Phase 2 | Memory Store（SQLite） | - [ ] 未开始 | | |
| Phase 3 | LLM 抽象层 + 两个 Client | - [ ] 未开始 | | |
| Phase 4 | Tool Registry + 6 个内置工具 | - [ ] 未开始 | | |
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
| 1.1 | 更新 Cargo.toml 依赖 | - [ ] 未完成 | |
| 1.2 | 创建公共类型 types.rs | - [ ] 未完成 | |
| 1.3 | 创建配置模块 config.rs | - [ ] 未完成 | |
| 1.4 | 初始化 tracing 日志 | - [ ] 未完成 | |

**阶段完成标志:** `cargo run` 能加载配置并输出日志

---

## Phase 2: Memory Store（SQLite）

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 2.1 | 创建 schema.sql | - [ ] 未完成 | |
| 2.2 | 实现 MemoryStore | - [ ] 未完成 | |

**阶段完成标志:** 3 个单元测试通过（save/get history, search memory, empty history）

---

## Phase 3: LLM 抽象层 + 两个 Client

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 3.1 | 定义 LlmClient trait | - [ ] 未完成 | |
| 3.2 | 实现 OpenAI-compatible Client | - [ ] 未完成 | |
| 3.3 | 实现 Anthropic Client | - [ ] 未完成 | |
| 3.4 | LLM Client 工厂函数 | - [ ] 未完成 | |

**阶段完成标志:** `cargo check` 通过，两个 client 实现完整

---

## Phase 4: Tool Registry + 6 个内置工具

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 4.1 | Tool trait + ToolRegistry | - [ ] 未完成 | |
| 4.2 | shell_exec 工具 | - [ ] 未完成 | |
| 4.3 | web_fetch 工具 | - [ ] 未完成 | |
| 4.4 | file_read + file_write 工具 | - [ ] 未完成 | |
| 4.5 | memory_save + memory_search 工具 | - [ ] 未完成 | |
| 4.6 | 工具注册工厂 | - [ ] 未完成 | |

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
