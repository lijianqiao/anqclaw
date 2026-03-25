# anqclaw

anqclaw 是一个用 Rust 构建的私人 AI 助理，支持飞书接入、CLI 对话、多 LLM、工具调用与持久记忆。

## 核心能力

- 多 LLM Profile（Anthropic / OpenAI-compatible / Ollama 等）
- Agentic Loop（LLM 与工具多轮协作）
- 内置工具：shell、web、file、memory
- SQLite 对话历史与长期记忆（FTS5）
- 飞书通道（可选启用）
- CLI 子命令：serve、chat、onboard、config show、config validate
- 配置与数据目录分离：`~/.anqclaw/`

## 架构概览

主链路：

Feishu/CLI Channel -> Gateway -> AgentCore -> ToolRegistry/MemoryStore -> Channel

核心模块：

- `channel`：飞书与 CLI 输入输出
- `gateway`：路由、去重、会话串行
- `agent`：上下文拼装与 agentic loop
- `llm`：多 Provider 抽象与客户端
- `tool`：工具注册与并发执行
- `memory`：SQLite 历史与长期记忆

## 快速开始

1. 构建：

```bash
cd agent
cargo build
```

2. 初始化（推荐首次使用）：

```bash
cargo run -- onboard
```

3. CLI 对话：

```bash
cargo run -- chat "你好"
# 或交互模式
cargo run -- chat
```

4. 启动服务模式：

```bash
cargo run -- serve
```

5. 配置查看与校验：

```bash
cargo run -- config show
cargo run -- config validate
```

## 文档

- 架构设计：[docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md](docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md)
- v1 计划：[docs/superpowers/plans/2026-03-24-anqclaw-v1-plan.md](docs/superpowers/plans/2026-03-24-anqclaw-v1-plan.md)
- v1 进度：[docs/superpowers/plans/2026-03-24-anqclaw-v1-progress.md](docs/superpowers/plans/2026-03-24-anqclaw-v1-progress.md)
- v2 计划与完成记录：[docs/superpowers/plans/2026-03-25-anqclaw-v2-plan.md](docs/superpowers/plans/2026-03-25-anqclaw-v2-plan.md)
