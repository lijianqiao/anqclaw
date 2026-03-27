# anqclaw

anqclaw 是一个用 Rust 构建的私人 AI 助理，当前支持飞书、HTTP 和 CLI 三种接入方式，具备多 LLM 协作、工具调用、持久记忆，以及面向真实任务的运行时自举能力。

## 当前能力

- 多 LLM Profile：Anthropic、OpenAI-compatible、Ollama 等
- Agentic Loop：LLM 与工具多轮协作，支持 tool calling 与流式回复
- 多通道接入：Feishu、HTTP API、CLI
- 内置工具：shell、web、file、memory、pdf_read、image_info、custom tool
- SQLite 对话历史与长期记忆，长期记忆采用 source table + FTS5 索引镜像
- Python 任务自举：支持在工作区自动准备 `.venv` 并执行脚本
- 默认安全收敛：受控 shell、文件沙箱、SSRF 校验、审计日志

## 架构概览

主链路：

Feishu/HTTP/CLI Channel -> Gateway -> AgentCore -> ToolRegistry/MemoryStore -> Channel

核心模块：

- `channel`：飞书、HTTP、CLI 输入输出
- `gateway`：路由、去重、限流、会话串行
- `agent`：上下文拼装、环境探测、agentic loop
- `llm`：多 Provider 抽象与客户端
- `tool`：工具注册与执行
- `memory`：SQLite 历史与长期记忆
- `audit` / `metrics` / `scheduler`：审计、指标与后台任务

## 快速开始

1. 构建

```bash
cd agent
cargo build
```

2. 首次初始化

```bash
cargo run -- onboard
```

3. CLI 对话

```bash
cargo run -- chat "你好"
# 或交互模式
cargo run -- chat
```

4. 启动服务

```bash
cargo run -- serve
```

5. 查看或校验配置

```bash
cargo run -- config show
cargo run -- config validate
```

## 质量状态

- 本地已完成 `cargo test --manifest-path agent/Cargo.toml`
- 项目已接入 CI，执行测试、clippy 和 cargo-audit
- 最近一轮修复已覆盖 custom tool、trusted path、web SSRF、stream 中断、Feishu token 刷新、长期记忆并发写入等回归场景

## 文档

- 自主能力链设计：[docs/autonomous-capability-chain-design.md](docs/autonomous-capability-chain-design.md)
- 基础架构设计基线：[docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md](docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md)
- 文件提取设计：[docs/superpowers/specs/2026-03-26-file-extraction-design.md](docs/superpowers/specs/2026-03-26-file-extraction-design.md)
