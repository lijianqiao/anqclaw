# anqclaw

anqclaw 是一个用 Rust 构建的私人 AI 助理。

目标是通过飞书 WebSocket 收发消息，结合多 LLM、工具调用、SQLite 持久记忆和定时任务，形成可长期运行的个人智能助手。

## 核心能力

- 飞书消息接入（WebSocket）与回复（REST API）
- 多 LLM 支持：Anthropic + OpenAI-compatible
- Agentic Loop：LLM 与工具多轮协作
- 内置 6 个工具：shell、web、file、memory
- SQLite 对话历史与长期记忆（FTS5）
- Heartbeat 定时任务

## 架构概览

消息主链路：

Feishu Channel -> Gateway -> AgentCore -> Tool Registry / MemoryStore -> Feishu Channel

模块职责：

- channel：平台接入（飞书）
- gateway：消息路由、去重、按会话串行
- agent：上下文构建与 agentic loop
- llm：统一抽象与具体 Provider 客户端
- tool：工具注册与并发执行
- memory：SQLite 历史与长期记忆
- heartbeat：定时触发任务

## 项目结构

主工程位于 [agent](agent)。

设计文档位于 [docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md](docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md)。

实施计划位于 [docs/superpowers/plans/2026-03-24-anqclaw-v1-plan.md](docs/superpowers/plans/2026-03-24-anqclaw-v1-plan.md)。

进度追踪位于 [docs/superpowers/plans/2026-03-24-anqclaw-v1-progress.md](docs/superpowers/plans/2026-03-24-anqclaw-v1-progress.md)。

## 快速启动

1. 进入工程目录。
2. 配置 [agent/config.toml](agent/config.toml)。
3. 设置必要环境变量（如 ANQ_LLM_API_KEY、ANQ_FEISHU_APP_SECRET）。
4. 运行：

```bash
cd agent
cargo check
cargo run
```

## 当前进度

- 已完成：Phase 1 到 Phase 5
- 进行中：Phase 6 及后续（Channel、Gateway、Heartbeat、入口整合、集成测试）
