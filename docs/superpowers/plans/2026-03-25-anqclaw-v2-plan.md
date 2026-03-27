# anqclaw v2 实施计划 — 多 LLM + 配置分离 + CLI 增强

> Created: 2026-03-25

---

## Phase 1: `~/.anqclaw/` 目录结构 + 跨平台 Home

**目标：** 配置、工作区、数据与项目代码分离，统一到 `~/.anqclaw/`。

**Files:**
- Add dep: `dirs` crate
- Create: `src/paths.rs` — 集中路径解析
- Modify: `src/config.rs` — 相对路径基于 anqclaw_home 解析 + `[feishu]` 可选化
- Modify: `src/main.rs` — 配置查找优先级链
- Modify: `src/lib.rs` — 注册新模块

### Task 1.1: 新增 `src/paths.rs`

- `anqclaw_home() -> PathBuf` — `dirs::home_dir().join(".anqclaw")`
- `resolve_path(base: &Path, relative: &str) -> PathBuf` — 绝对路径不变，相对路径基于 base 解析
- `ensure_dirs(home: &Path)` — 确保 workspace/ data/ sessions/ skills/ logs/ 子目录存在
- `config_search_chain(cli_path: Option<&str>) -> PathBuf` — 4 级优先级搜索

### Task 1.2: 修改 `config.rs` — 默认路径 + feishu 可选

- 默认 `workspace = "workspace"`, `db_path = "data/memory.db"` (相对路径)
- `[feishu]` 改为 `Option<RawFeishuSection>` — 不配置则跳过飞书
- `AppConfig` 中 `feishu: Option<FeishuSection>`
- `log_file` 字段加入 `[app]` section (可选)

### Task 1.3: 修改 `main.rs` — 路径解析 + feishu 条件启动

- 启动时先 `ensure_dirs()` 创建目录结构
- 所有相对路径基于 `anqclaw_home()` 解析
- 无 feishu 配置时跳过 FeishuChannel

### ✅ Phase 1 验收
- `cargo build` 通过
- 无 feishu 配置时程序仍能启动
- 路径正确解析到 `~/.anqclaw/`

---

## Phase 2: 修复 openai_compat + api_key 可选 + 多 LLM Profile

**目标：** 修复当前 bug，支持 Ollama/Gemini/OpenAI 等，多 profile 配置。

**Files:**
- Modify: `src/llm/openai_compat.rs` — 智能 URL 拼接 + auth 可选 + supports_tools
- Modify: `src/config.rs` — 多 LLM profile: `HashMap<String, LlmProfileSection>`
- Modify: `src/llm/mod.rs` — 工厂函数从 profile 创建
- Modify: `src/main.rs` — 从 agent.llm_profile 选择 profile

### Task 2.1: 修复 `openai_compat.rs`

- base_url 智能拼接：检测是否已含 `/v1/chat/completions` 或 `/v1`
- api_key 为空时跳过 `Authorization` header (Ollama 场景)
- 新增 `supports_tools` 配置项 (默认 true)，false 时不传 tools 字段
- 新增 `max_tokens_field_name` 配置项 (默认 `"max_tokens"`)，Gemini 可设为 `"max_output_tokens"`

### Task 2.2: 多 LLM Profile 配置

配置结构变化：

```toml
# 新格式：多 profile
[llm.default]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "${ANTHROPIC_API_KEY}"

[llm.deepseek]
provider = "openai_compat"
model = "deepseek-chat"
api_key = "${DEEPSEEK_API_KEY}"
base_url = "https://api.deepseek.com"

[llm.ollama]
provider = "openai_compat"
model = "carstenuhlig/omnicoder-9b"
base_url = "http://localhost:11434"

[agent]
llm_profile = "default"
```

- `AppConfig.llm` 从 `LlmSection` 改为 `HashMap<String, LlmSection>`
- `AgentSection` 增加 `llm_profile: String` (默认 "default")
- 向下兼容：如果 TOML 中 `[llm]` 直接有 provider/model 等字段（非嵌套），视为 `[llm.default]`

### Task 2.3: 工厂函数 + main.rs 适配

- `create_llm_client(profiles, profile_name)` — 从 HashMap 中取对应 profile
- main.rs 中按 `config.agent.llm_profile` 创建 client

### ✅ Phase 2 验收
- Ollama (无 key)、DeepSeek、Gemini、Anthropic 都能正确构建 client
- 旧格式单 `[llm]` 配置仍兼容
- `cargo test` 通过

---

## Phase 3: CLI 子命令框架 + `anqclaw chat`

**目标：** 增加子命令结构，实现 CLI 对话模式。

**Files:**
- Modify: `src/main.rs` — clap Subcommand 重构
- Create: `src/channel/cli.rs` — CLI Channel (stdin/stdout)
- Modify: `src/channel/mod.rs` — 注册 CliChannel

### Task 3.1: CLI 子命令结构

```
anqclaw                         # 默认 = serve
anqclaw serve [-c config.toml]  # 启动飞书 WS 服务
anqclaw chat  [-c config.toml] [message]  # CLI 对话模式
anqclaw onboard                 # 交互式初始化
anqclaw config show             # 显示配置（脱敏）
anqclaw config validate         # 验证配置
```

### Task 3.2: CliChannel + `chat` 子命令

- `CliChannel` 实现 `Channel` trait
- 单次模式: `anqclaw chat "hello"` → 回复后退出
- 交互模式: `anqclaw chat` (无参数) → REPL 循环
- 不走飞书，直接 stdin→Agent→stdout

### ✅ Phase 3 验收
- `anqclaw chat "test"` 能单次对话
- `anqclaw chat` 能交互 REPL
- `anqclaw serve` 等价于原行为

---

## Phase 4: `anqclaw onboard` 交互式初始化

**目标：** 引导用户完成首次配置。

**Files:**
- Add dep: `dialoguer` crate
- Create: `src/cli/mod.rs` — CLI 子命令逻辑
- Create: `src/cli/onboard.rs` — Onboard 交互向导

### Task 4.1: Onboard 交互向导

5 步流程:
1. 创建 `~/.anqclaw/` 目录结构
2. LLM 配置 (选择 provider → 输入 key/model)
3. 飞书配置 (可选跳过)
4. 生成 workspace 模板文件
5. 可选连通性验证

### ✅ Phase 4 验收
- `anqclaw onboard` 完成后 `~/.anqclaw/config.toml` 正确生成
- workspace 模板文件就位
- 配置可直接用于 `anqclaw serve` 或 `anqclaw chat`

---

## Phase 5: `anqclaw config show/validate` + 日志文件

**目标：** 配置管理工具 + 文件日志。

**Files:**
- Create: `src/cli/config_cmd.rs` — config 子命令
- Modify: `src/main.rs` — 日志文件输出

### Task 5.1: `config show` + `config validate`

- `show`: 加载配置，API key 脱敏显示 (只显示前4位+****)
- `validate`: 检查文件存在、env var 已设置、LLM 端点可达、飞书凭据有效

### Task 5.2: 日志文件输出

```toml
[app]
log_file = "logs/anqclaw.log"  # 可选
```

- 使用 `tracing-appender` 的 `RollingFileAppender`
- stderr + 文件双写

### ✅ Phase 5 验收
- `anqclaw config show` 正确显示脱敏配置
- `anqclaw config validate` 检查并报告问题
- 日志文件正确写入

---

## Phase 6: 最终验证

- `cargo test` 全部通过
- `cargo clippy -- -D warnings` 无警告
- `cargo build --release` 成功
- 端到端测试: onboard → chat → serve

---

## 进度追踪

| Phase | 内容 | 状态 | 完成日期 | 备注 |
|-------|------|------|----------|------|
| Phase 1 | ~/.anqclaw/ + 跨平台 + feishu 可选 | - [x] 已完成 | 2026-03-25 | paths.rs + dirs crate + feishu Optional |
| Phase 2 | 修复 openai_compat + 多 LLM Profile | - [x] 已完成 | 2026-03-25 | 智能 URL + api_key 可选 + HashMap profiles |
| Phase 3 | CLI 子命令 + anqclaw chat | - [x] 已完成 | 2026-03-25 | serve/chat/onboard/config 子命令 + CliChannel |
| Phase 4 | anqclaw onboard | - [x] 已完成 | 2026-03-25 | dialoguer 交互向导 + 8 provider preset |
| Phase 5 | config show/validate + 日志文件 | - [x] 已完成 | 2026-03-25 | config show 脱敏 + validate 检查 |
| Phase 6 | 最终验证 | - [x] 已完成 | 2026-03-25 | 45 tests + clippy 0 warning + release build |

## 变更记录

| 日期 | 变更内容 |
|------|----------|
| 2026-03-25 | 创建 v2 实施计划 |
| 2026-03-25 | Phase 1-6 全部完成：~/.anqclaw/ + 多 LLM + CLI chat + onboard + config 管理 |
