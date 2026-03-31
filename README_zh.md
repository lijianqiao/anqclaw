# anqclaw

[English Version](README.md)

anqclaw 是一个用 Rust 构建的私人 AI 助理，当前支持飞书、HTTP 和 CLI 三种接入方式，具备多 LLM 协作、工具调用、持久记忆，以及面向真实任务的运行时自举能力。

## 当前能力

- 多 LLM Profile：Anthropic、OpenAI-compatible、Ollama 等
- Agentic Loop：LLM 与工具多轮协作，支持 tool calling 与流式回复
- 多通道接入：Feishu、HTTP API、CLI
- Skills 主链：候选 skill 以结构化 `<available_skills>` 暴露，模型按需读取 `SKILL.md`
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
- `skill`：多源 skills 扫描、候选摘要生成与热重载
- `memory`：SQLite 历史与长期记忆
- `audit` / `metrics` / `scheduler`：审计、指标与后台任务

## Skills 主链

- Skill 包采用目录式结构：`skills/<name>/SKILL.md`
- Skills 来源按 `bundled -> user(~/.anqclaw/skills) -> workspace(<workspace>/skills_dir)` 合并，后加载来源覆盖先加载来源
- Agent 会先按 `description` 做自动候选匹配，并结合 `keywords`、`trigger`、`extensions`、历史文件名和 workspace 扩展名做增强排序，再把可读路径以结构化 `<available_skills>` 注入 system prompt，而不是直接注入 skill 正文
- 模型命中 skill 后，主路径是通过 `file_read` 按需读取对应 `SKILL.md`；`activate_skill` 仅保留为兼容或调试入口
- `serve` 模式下支持技能目录热重载，并记录触发 reload 的文件路径，便于审计

## 部署

前提：

- 已拿到与你的系统和 CPU 架构匹配的发布文件
- 已准备配置文件
- 程序目录和数据目录可读写
- 目标机器能访问所用的 LLM 服务和通道依赖

### Windows

推荐路径：

```text
C:\anqclaw\anqclaw.exe
```

可选：将 `C:\anqclaw\` 加入 PATH。

已加入 PATH：

```powershell
anqclaw.exe onboard
anqclaw.exe config validate
anqclaw.exe serve
```

未加入 PATH：

```powershell
C:\anqclaw\anqclaw.exe onboard
C:\anqclaw\anqclaw.exe config validate
C:\anqclaw\anqclaw.exe serve
```

按需：

- 安装 Microsoft Visual C++ Redistributable
- 如果启用 Python 自举或自动装包，安装 Python 或 `uv`
- 如果 prompt 或 custom tool 依赖外部命令，安装对应命令

### Linux

推荐路径：

```text
/opt/anqclaw/anqclaw
```

准备：

```bash
chmod +x /opt/anqclaw/anqclaw
ln -sf /opt/anqclaw/anqclaw /usr/local/bin/anqclaw
```

已加入 PATH：

```bash
anqclaw onboard
anqclaw config validate
anqclaw serve
```

未加入 PATH：

```bash
/opt/anqclaw/anqclaw onboard
/opt/anqclaw/anqclaw config validate
/opt/anqclaw/anqclaw serve
```

按需：

- 如果启用 Python 自举或自动装包，安装 Python 或 `uv`
- 如果 prompt 或 custom tool 依赖外部命令，安装对应命令

### macOS

推荐路径：

```text
/usr/local/anqclaw/anqclaw
```

准备：

```bash
chmod +x /usr/local/anqclaw/anqclaw
ln -sf /usr/local/anqclaw/anqclaw /usr/local/bin/anqclaw
```

已加入 PATH：

```bash
anqclaw onboard
anqclaw config validate
anqclaw serve
```

未加入 PATH：

```bash
/usr/local/anqclaw/anqclaw onboard
/usr/local/anqclaw/anqclaw config validate
/usr/local/anqclaw/anqclaw serve
```

按需：

- 首次运行如被系统阻止，执行 `xattr -d com.apple.quarantine /usr/local/anqclaw/anqclaw`
- 如果启用 Python 自举或自动装包，安装 Python 或 `uv`
- 如果 prompt 或 custom tool 依赖外部命令，安装对应命令

## 开发

只有改代码、调试、跑本地开发流程时才需要这一节。

前提：

- `rustup`、`rustc`、`cargo`
- 平台构建工具链
  - Windows：Visual Studio Build Tools / MSVC
  - Linux：`gcc` 或 `clang`

常用命令：

```bash
cd agent
cargo build
cargo run -- onboard
cargo run -- chat
cargo run -- serve
cargo run -- config validate
```

## 从源码构建发布版

前提：已安装 Rust 工具链。

构建：

```bash
cd agent
cargo build --release
```

产物路径：

- Windows：`agent/target/release/anqclaw.exe`
- Linux/macOS：`agent/target/release/anqclaw`

## 质量状态

- 本地已完成 `cargo test --manifest-path agent/Cargo.toml`
- 本地验证也包括 `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo audit` 已纳入本地依赖检查；当前仍有少量来自上游传递依赖的告警
- 最近一轮修复已覆盖 custom tool、trusted path、web SSRF、stream 中断、Feishu token 刷新、长期记忆并发写入，以及 skills 候选筛选与按需读取主链等回归场景

## 文档

- 自主能力链设计：[docs/autonomous-capability-chain-design.md](docs/autonomous-capability-chain-design.md)
- 基础架构设计基线：[docs/2026-03-24-anqclaw-v1-design.md](docs/2026-03-24-anqclaw-v1-design.md)
- 文件提取设计：[docs/2026-03-26-file-extraction-design.md](docs/2026-03-26-file-extraction-design.md)