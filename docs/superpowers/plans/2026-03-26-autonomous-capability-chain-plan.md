# anqclaw — 自主能力链实施计划

> Created: 2026-03-26
> Design: `docs/autonomous-capability-chain-design.md`

**Goal:** 让 anqclaw 具备环境感知、结构化错误分类、自动安装策略和管道安全解析能力，从"盲目试错"进化为"环境感知 → 智能分类 → 自主修复"。

**进度追踪:** `docs/superpowers/plans/2026-03-26-autonomous-capability-chain-progress.md`

---

## 阶段总览

| 阶段 | 内容 | 新增文件 | 修改文件 | 预估改动量 |
|------|------|----------|----------|-----------|
| Phase 1 | 环境探测 + Prompt 重构 | `src/agent/probe.rs` | `config.rs`, `prompt.rs`, `context.rs`, `agent/mod.rs`, `lib.rs` | ~250 行 |
| Phase 2 | 结构化错误分类 | `src/tool/error_classifier.rs` | `agent/mod.rs`, `tool/mod.rs` | ~340 行 |
| Phase 3 | 连续错误保护 + 管道安全 | — | `agent/mod.rs`, `tool/shell.rs` | ~185 行 |

---

## Phase 1: 环境探测 + Prompt 重构

**目标：** 启动时探测主机环境（python/pip/uv/node 等），动态生成 Runtime Environment 节注入 system prompt，替代硬编码的文件处理指令。

**Files:**
- Create: `src/agent/probe.rs`
- Modify: `src/config.rs` — AgentSection 增加 4 个字段
- Modify: `src/agent/prompt.rs` — 移除硬编码 File Handling 节
- Modify: `src/agent/context.rs` — `build_system_prompt()` 接收 `&EnvironmentProbe`
- Modify: `src/agent/mod.rs` — AgentCore 启动时执行探测，存储到字段
- Modify: `src/lib.rs` — 无需改（probe.rs 在 agent 子模块内）

### Task 1.1: config.rs — AgentSection 新增配置项

- [ ] **Step 1: 增加字段和默认值函数**

在 `AgentSection` struct 中新增 4 个字段：

```rust
#[serde(default)]
pub auto_install_packages: bool,              // 默认 false

#[serde(default = "default_install_scope")]
pub install_scope: String,                    // 默认 "venv"

#[serde(default = "default_venv_path")]
pub venv_path: String,                        // 默认 ".anqclaw/envs"

#[serde(default = "default_max_consecutive_tool_errors")]
pub max_consecutive_tool_errors: u32,         // 默认 3

#[serde(default)]
pub probe_extra_binaries: Vec<String>,        // 默认空
```

- [ ] **Step 2: 补充 Default impl 和默认值函数**

新增 `default_install_scope()`, `default_venv_path()`, `default_max_consecutive_tool_errors()` 函数。更新 `Default for AgentSection`。

- [ ] **Step 3: `cargo check` 确认编译通过**

### Task 1.2: 新增 src/agent/probe.rs — EnvironmentProbe

- [ ] **Step 1: 创建文件，定义 `BinaryInfo` 和 `EnvironmentProbe` struct**

```rust
pub struct BinaryInfo {
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
}

pub struct EnvironmentProbe {
    pub binaries: HashMap<String, BinaryInfo>,
}
```

- [ ] **Step 2: 实现 `detect()` 异步方法**

- 并发探测所有 `PROBE_BINARIES`（+ config 的 `probe_extra_binaries`）
- Windows 用 `where`，Unix 用 `which`
- 对可用二进制执行 `--version` 获取版本
- 单个探测超时 2 秒
- Windows 特殊处理：python3 不存在时，检查 python --version 是否为 3.x，虚拟映射 python3=available

- [ ] **Step 3: 实现 `to_prompt_section()` — 生成 Runtime Environment 节**

- 列出每个 binary 的可用状态和版本
- 根据 python/pip/uv 可用性生成 Environment Guidelines
- 根据 `auto_install_packages` 和 `install_scope` 生成 Package Installation Policy

- [ ] **Step 4: 实现 `has()` 辅助方法**

- [ ] **Step 5: 在 agent/mod.rs 中注册模块**

`agent/mod.rs` 顶部增加 `pub mod probe;`

- [ ] **Step 6: `cargo check` 确认编译通过**

### Task 1.3: prompt.rs — 移除硬编码 File Handling

- [ ] **Step 1: 删除 `## File Handling` 整节**

从 `DEFAULT_SYSTEM_PROMPT` 中移除以下内容：

```
## File Handling

- When asked to read a PDF file, use the `pdf_read` tool.
- When asked about an image, use `image_info` to get format, dimensions, and optionally base64 data.
- When asked to read a .docx file, use `shell_exec` with: ...
- When asked to read a .xlsx file, use `shell_exec` with: ...
- If Python packages are not available, inform the user to install ...
```

保留 `pdf_read` 和 `image_info` 的说明（这些是内建工具，不依赖外部环境），可以改为简洁的一行引导，或完全移除（工具定义自带描述）。

> **决策点：** `pdf_read` / `image_info` 的使用提示可保留在 DEFAULT_SYSTEM_PROMPT 中，也可移至 `to_prompt_section()` 根据工具注册状态动态生成。推荐先简单处理——仅移除 .docx/.xlsx 的硬编码 python3 命令，保留 pdf_read/image_info 提示。

### Task 1.4: context.rs — build_system_prompt 接收 EnvironmentProbe

- [ ] **Step 1: 修改 `build_system_prompt` 签名**

```rust
pub fn build_system_prompt(
    config: &AppConfig,
    skill_summary: &str,
    env_probe: &EnvironmentProbe,  // 新增参数
) -> String
```

- [ ] **Step 2: 在 prompt 拼装末尾注入 env_probe.to_prompt_section()**

在 skill_summary 之后、return 之前，追加 `env_probe.to_prompt_section(&config.agent)`:

```rust
let env_section = env_probe.to_prompt_section(&config.agent);
if !env_section.is_empty() {
    prompt.push_str("\n\n---\n\n");
    prompt.push_str(&env_section);
}
```

- [ ] **Step 3: 更新所有 `build_system_prompt` 调用点**

在 `agent/mod.rs` 的 `do_handle` 中传入 `&self.env_probe`。

### Task 1.5: agent/mod.rs — AgentCore 集成 EnvironmentProbe

- [ ] **Step 1: AgentCore 新增字段 `env_probe: EnvironmentProbe`**

- [ ] **Step 2: `AgentCore::new()` 中调用 `EnvironmentProbe::detect(&config.agent).await`**

传入 `probe_extra_binaries` 配置。

- [ ] **Step 3: `do_handle()` 中 `build_system_prompt()` 调用处传入 `&self.env_probe`**

- [ ] **Step 4: `cargo check` 确认编译通过**

### Task 1.6: 单元测试 — EnvironmentProbe

- [ ] **Step 1: probe.rs 底部增加 `#[cfg(test)] mod tests`**

测试项：
- `test_to_prompt_section_with_python` — 构造有 python3+pip 的 probe，验证生成的 section 包含关键字
- `test_to_prompt_section_without_python` — 无 python，验证生成提示安装
- `test_to_prompt_section_install_policy_enabled` — auto_install=true，验证 venv 指导
- `test_to_prompt_section_install_policy_disabled` — auto_install=false，验证禁止安装提示
- `test_has_method` — 测试 `has()` 对存在/不存在 binary 的返回值

- [ ] **Step 2: `cargo test` 全部通过**

### ✅ Phase 1 验收
- `cargo check --all-targets` 通过
- probe 相关 5+ 个单元测试通过
- 启动时能看到环境探测日志
- `DEFAULT_SYSTEM_PROMPT` 不再包含硬编码 python3 命令
- `build_system_prompt()` 输出包含 `## Runtime Environment` 节

---

## Phase 2: 结构化错误分类

**目标：** 工具执行失败后，在 tool result 末尾附加结构化错误分类和修复提示，帮 LLM 更快定位问题。

**Files:**
- Create: `src/tool/error_classifier.rs`
- Modify: `src/tool/mod.rs` — 注册新模块
- Modify: `src/agent/mod.rs` — tool result 处理后触发分类器

### Task 2.1: 新增 src/tool/error_classifier.rs

- [ ] **Step 1: 定义 `ToolErrorKind` 枚举和 `ErrorClassification` struct**

```rust
pub enum ToolErrorKind {
    CommandNotFound { command: String },
    ModuleNotFound { module: String, language: String },
    PermissionDenied,
    Timeout,
    SyntaxError { language: String },
    NetworkError,
    FileNotFound { path: String },
    DiskFull,
    Unknown,
}

pub struct ErrorClassification {
    pub kind: ToolErrorKind,
    pub hint: Option<String>,
}
```

- [ ] **Step 2: 实现 `classify_error()` 函数**

参数：`tool_name: &str, output: &str, exit_code: Option<i32>, env: &EnvironmentProbe`

分类规则按优先级：
1. exit_code 127/9009 或 "command not found" → `CommandNotFound`
2. "ModuleNotFoundError" / "No module named" → `ModuleNotFound` (python)
3. "Cannot find module" / "MODULE_NOT_FOUND" → `ModuleNotFound` (node)
4. "Permission denied" / "EACCES" / "Access is denied" → `PermissionDenied`
5. "SyntaxError" / "IndentationError" → `SyntaxError`
6. "No such file or directory" / "FileNotFoundError" → `FileNotFound`
7. "ConnectionRefusedError" / "Could not resolve host" → `NetworkError`
8. "No space left on device" / "ENOSPC" → `DiskFull`
9. 其他 → `Unknown`

- [ ] **Step 3: 实现 `format_error_annotation()` 函数**

输出格式：`\n\n[error_type: xxx]\n[hint: yyy]`

- [ ] **Step 4: 实现辅助函数**

- `extract_missing_command(output)` — 从 "xxx: command not found" 提取命令名
- `extract_module_name(output)` — 从 "No module named 'xxx'" 提取模块名
- `extract_node_module_name(output)` — 从 "Cannot find module 'xxx'" 提取模块名
- `extract_file_path(output)` — 从错误信息提取文件路径
- `suggest_install_command(command, env)` — 根据环境建议安装方式
- `parse_exit_code(output)` — 从 `[exit code: N]` 解析 exit code

- [ ] **Step 5: tool/mod.rs 注册模块**

`pub mod error_classifier;`

- [ ] **Step 6: `cargo check` 确认编译通过**

### Task 2.2: agent/mod.rs — 集成 ErrorClassifier

- [ ] **Step 1: 在 `do_handle` 的 tool result 循环中触发分类**

在 `execute_batch` 返回后，遍历 `(call, result)` 对：

```rust
let should_classify = result.is_error
    || (call.name == "shell_exec"
        && result.output.contains("[exit code:")
        && !result.output.contains("[exit code: 0]"));

if should_classify {
    let exit_code = error_classifier::parse_exit_code(&result.output);
    let classification = error_classifier::classify_error(
        &call.name, &result.output, exit_code, &self.env_probe,
    );
    if classification.kind != ToolErrorKind::Unknown {
        result.output += &error_classifier::format_error_annotation(&classification);
    }
}
```

- [ ] **Step 2: `cargo check` 确认编译通过**

### Task 2.3: 单元测试 — ErrorClassifier

- [ ] **Step 1: error_classifier.rs 底部增加测试模块**

测试项：
- `test_classify_command_not_found_unix` — exit_code=127 + "command not found"
- `test_classify_command_not_found_windows` — exit_code=9009 + "is not recognized"
- `test_classify_module_not_found_python` — "ModuleNotFoundError: No module named 'pandas'"
- `test_classify_module_not_found_node` — "Cannot find module 'express'"
- `test_classify_permission_denied` — "Permission denied"
- `test_classify_syntax_error` — "SyntaxError: invalid syntax"
- `test_classify_file_not_found` — "FileNotFoundError" / "No such file or directory"
- `test_classify_network_error` — "ConnectionRefusedError"
- `test_classify_disk_full` — "No space left on device"
- `test_classify_unknown` — 不匹配任何模式
- `test_parse_exit_code` — 从 "[exit code: 1]" 解析出 Some(1)
- `test_format_error_annotation` — 验证输出格式
- `test_hint_with_uv_available` — 有 uv 时建议 uv pip install
- `test_hint_without_pip` — 无 pip 时提示用户安装

- [ ] **Step 2: `cargo test` 全部通过**

### ✅ Phase 2 验收
- `cargo check --all-targets` 通过
- error_classifier 相关 14+ 个单元测试通过
- shell_exec 返回 exit_code!=0 时，tool result 末尾附加 `[error_type: ...]` 和 `[hint: ...]`
- is_error=true 的其他工具错误也能被分类

---

## Phase 3: 连续错误保护 + 管道安全

**目标：** 防止 LLM 在错误循环中反复尝试浪费 token；拦截管道/链式命令中的危险子命令。

**Files:**
- Modify: `src/agent/mod.rs` — do_handle 增加连续错误计数
- Modify: `src/tool/shell.rs` — 增加 split_command_chain + check_command_chain

### Task 3.1: agent/mod.rs — 连续错误保护

- [ ] **Step 1: do_handle 中增加连续错误计数**

在 agentic loop 开始前初始化：

```rust
let mut consecutive_errors: usize = 0;
let max_consecutive = self.config.agent.max_consecutive_tool_errors as usize;
```

在 tool_results 处理后判断:

```rust
// per-round 全失败计数：一轮中所有工具都失败时 +1，任何一个成功就重置
// shell_exec 特殊处理：exit_code!=0 也视为失败（即使 is_error=false）
let all_failed = tool_results.iter().all(|r| {
    r.is_error
        || (r.output.contains("[exit code:")
            && !r.output.contains("[exit code: 0]"))
});
if all_failed {
    consecutive_errors += 1;
    if consecutive_errors >= max_consecutive {
        // 注入系统提示，告诉 LLM 停止重试
        let stop_hint = format!(
            "[system: {} consecutive tool failures detected. \
             Stop retrying and summarize the problem to the user. \
             Suggest manual steps they can take to resolve it.]",
            consecutive_errors
        );
        // 以 user message 或 system annotation 形式追加
    }
} else {
    consecutive_errors = 0;
}
```

- [ ] **Step 2: `cargo check` 确认编译通过**

### Task 3.2: shell.rs — 管道命令解析

- [ ] **Step 1: 实现 `split_command_chain(command: &str) -> Vec<String>`**

- 按 `|`, `&&`, `||` 分隔（所有平台通用）
- Unix 额外分隔 `;`
- Windows 额外分隔 `&`（单个 `&` 是无条件链式执行，注意区分 `&&`）
- 跳过引号内的分隔符（单引号、双引号）
- 返回每个子命令的 trim 结果

- [ ] **Step 2: 实现 `check_command_chain(command: &str) -> Result<()>`**

对 `split_command_chain` 的每个子命令：
- 提取 first_token
- 对每个 token 应用现有的 blocked/allowlist 检查

- [ ] **Step 3: 替换 `do_execute` 中现有的 first_token 检查逻辑**

将原有直接提取 first_token + 检查的代码替换为调用 `check_command_chain()`。

- [ ] **Step 4: `cargo check` 确认编译通过**

### Task 3.3: 单元测试

- [ ] **Step 1: shell.rs 测试模块增加管道解析测试**

测试项：
- `test_split_simple_pipe` — `"ls | grep foo"` → `["ls", "grep foo"]`
- `test_split_and_chain` — `"echo a && echo b"` → `["echo a", "echo b"]`
- `test_split_or_chain` — `"cmd1 || cmd2"` → `["cmd1", "cmd2"]`
- `test_split_semicolon_unix` — `"echo a; echo b"` → Unix 2 段 / Windows 1 段
- `test_split_ampersand_windows` — `"echo a & echo b"` → Windows 2 段 / Unix 1 段
- `test_split_preserves_quotes` — `"echo 'a | b' | cat"` → `["echo 'a | b'", "cat"]`
- `test_check_chain_blocks_dangerous` — `"echo hello | rm -rf /"` → 被 blocked
- `test_check_chain_allows_safe` — `"echo hello | grep world"` → 通过

- [ ] **Step 2: agent/mod.rs 测试增加连续错误保护测试**

测试项：
- `test_consecutive_errors_resets_on_success` — 失败-失败-成功 → 计数归零
- `test_consecutive_errors_triggers_stop` — 连续 3 次全失败 → 注入停止提示

- [ ] **Step 3: `cargo test` 全部通过**

### ✅ Phase 3 验收
- `cargo check --all-targets` 通过
- 管道解析 8+ 个单元测试通过
- 连续错误保护 2+ 个测试通过
- `echo hello | rm -rf /` 在 Readonly/Supervised 模式下被拦截
- 连续 3 次工具全失败后，LLM 收到停止提示

---

## 全量验收清单

| 检查项 | 命令 |
|--------|------|
| 编译通过 | `cargo check --all-targets` |
| 全部测试通过 | `cargo test` |
| Clippy 检查 | `cargo clippy --all-targets` |
| Release 构建 | `cargo build --release` |
| 启动验证 | `cargo run -- --config ... serve` 能看到环境探测日志 |
| 功能验证 | 发送需要 Python 处理的任务，验证 Runtime Environment 节 + 错误分类 + 修复提示 |

---

## 依赖关系

```
Phase 1 (探测 + Prompt)
   │
   ├── Phase 2 (错误分类 — 依赖 EnvironmentProbe)
   │
   └── Phase 3 (连续错误 + 管道安全 — 依赖 config 新字段)
```

Phase 2 和 Phase 3 互不依赖，均依赖 Phase 1。可并行开发但建议顺序实施以降低冲突。
