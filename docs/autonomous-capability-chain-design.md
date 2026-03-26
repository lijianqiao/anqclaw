# anqclaw — 自主能力链设计方案

> Created: 2026-03-26
> Status: Draft
> Reference: OpenClaw 架构分析, anqclaw file-extraction-design

---

## 1. 问题

当前 anqclaw 在遇到需要外部环境的任务时（如处理 Excel、数据分析），存在三个缺陷：

1. **盲目试错**：LLM 不知道主机上有什么环境，必须先执行命令失败后才知道缺什么，浪费 2-3 轮 token
2. **无法自愈**：缺少依赖时只能告诉用户"请安装 xxx"，无法自主修复环境
3. **错误信息原始**：工具失败时直接透传 stderr，LLM 需要从大段错误文本中自己推理修复策略

理想行为：用户说"帮我汇总 sales.xlsx 的月度数据"，anqclaw 应该自主完成 **环境检测 → 依赖安装 → 代码生成 → 执行 → 验证 → 返回结果** 的完整链路。

## 2. 设计原则

基于 OpenClaw 架构分析，提炼出适合 anqclaw 的设计原则：

| 原则 | 说明 |
|------|------|
| **LLM 自主决策** | 系统不硬编码"遇到什么错误就做什么"的规则，修复策略由 LLM 推理决定 |
| **环境感知辅助** | 但系统应在启动时探测环境，**提前告知** LLM 可用能力，减少试错成本 |
| **结构化错误** | 错误透传时附加分类和提示，帮 LLM 更快定位问题，减少推理弯路 |
| **安全可控** | 自动安装必须可配置、有隔离，默认关闭 |
| **保持轻量** | 不引入 Docker 沙箱、插件系统等重型架构，保持单二进制优势 |

### 与 OpenClaw 的关键差异

| | OpenClaw | anqclaw |
|---|---|---|
| 环境感知 | 无预检，靠 LLM 试错 | **启动时探测，注入 prompt** |
| 错误恢复 | 原始 stderr 透传 | **结构化分类 + 环境上下文提示** |
| 安装依赖 | LLM 完全自主（无策略控制） | **可配置策略，默认安全** |
| 数据处理 | Python only via exec | Python only via shell_exec（一致） |
| 沙箱 | Docker/SSH/OpenShell | venv 隔离（轻量） |
| 安全 | 5 层策略管线 + 审批协议 | 三级权限 + 管道解析（够用） |

## 3. 架构总览

```
┌─────────────────────────────────────────────────────────┐
│                     启动阶段                              │
│  ┌──────────────┐                                        │
│  │EnvironmentProbe│ → 探测 python3/pip/uv/node           │
│  └──────┬───────┘                                        │
│         │ 结果                                           │
│         ▼                                                │
│  ┌──────────────┐                                        │
│  │PromptBuilder │ → 动态生成 ## Runtime Environment 节    │
│  └──────────────┘                                        │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│                     运行阶段（Agent Loop）                 │
│                                                          │
│  用户请求 → LLM 规划（已知环境能力）→ 生成代码             │
│       → shell_exec 执行 → 成功 → 返回结果                │
│                          → 失败 ↓                        │
│                    ┌─────────────────┐                    │
│                    │ErrorClassifier  │ → 分类 + 附加提示   │
│                    └────────┬────────┘                    │
│                             ↓                            │
│                    LLM 看到结构化错误                      │
│                    → 自主决定修复策略                      │
│                    → 安装依赖 / 换方案 / 告知用户          │
│                    → 重试（受 max_consecutive_errors 限制）│
└─────────────────────────────────────────────────────────┘
```

## 4. 详细设计

### 4.1 环境探测（EnvironmentProbe）

启动时通过 `which`/`where` 探测关键运行时，结果缓存在内存中。

**新增文件：** `src/agent/probe.rs`（探测逻辑）

**集成文件：** `src/agent/context.rs`（`build_system_prompt()` 接收 `&EnvironmentProbe`，在 workspace 文件之后插入环境节）

```rust
use std::collections::HashMap;

pub struct EnvironmentProbe {
    pub binaries: HashMap<String, BinaryInfo>,
}

pub struct BinaryInfo {
    pub available: bool,
    pub version: Option<String>,  // 从 --version 解析
    pub path: Option<String>,     // which/where 返回的路径
}

/// 默认探测列表
///
/// 平台差异：
/// - Unix: `which` 探测，python3 优先
/// - Windows: `where` 探测，Python 安装器只创建 `python.exe`（无 python3），
///   需检查 `python --version` 输出是否为 3.x
const PROBE_BINARIES: &[&str] = &[
    "python3", "python", "pip3", "pip", "uv",
    "node", "npm", "git", "curl",
];

impl EnvironmentProbe {
    /// 启动时调用，异步并发探测所有二进制
    pub async fn detect() -> Self {
        // 并发执行 which/where 命令（Windows 用 where，Unix 用 which）
        // 对可用的二进制，进一步执行 --version 获取版本
        // 超时 2 秒，单个探测失败不阻塞
        //
        // Windows 特殊处理：
        // - python3 在 Windows 上通常不存在，需探测 python 并检查版本
        // - 若 python --version 输出 "Python 3.x.x"，在结果中同时标记
        //   python3=available（虚拟映射），并记录实际命令为 "python"
        // - to_prompt_section() 中统一报告为 python3，注明实际路径
        todo!()
    }

    /// 生成注入 system prompt 的环境描述节
    pub fn to_prompt_section(&self) -> String {
        let mut s = String::from("## Runtime Environment\n\n");
        s += "The following runtimes are detected on this system:\n\n";

        for (name, info) in &self.binaries {
            if info.available {
                let ver = info.version.as_deref().unwrap_or("unknown version");
                s += &format!("- {name}: available ({ver})\n");
            } else {
                s += &format!("- {name}: NOT available\n");
            }
        }

        // 根据探测结果生成策略指导
        s += "\n### Environment Guidelines\n\n";

        let has_python = self.has("python3") || self.has("python");
        let has_pip = self.has("pip3") || self.has("pip");
        let has_uv = self.has("uv");

        if has_python && has_pip {
            s += "- Python is available with pip. You can install packages and run scripts.\n";
            s += "- For data processing (xlsx, csv, docx), write Python scripts and execute via shell_exec.\n";
            s += "- If a package is missing, install it first (prefer uv if available, else pip).\n";
        } else if has_python && !has_pip {
            s += "- Python is available but pip is NOT. You can run scripts with stdlib only.\n";
            s += "- If packages are needed, inform the user to install pip first.\n";
        } else {
            s += "- Python is NOT available on this system.\n";
            s += "- For tasks requiring Python, inform the user to install Python.\n";
        }

        if has_uv {
            s += "- uv is available. Prefer `uv pip install` over `pip install` for speed.\n";
            s += "- Use `uv venv` for isolated environments when installing packages.\n";
        }

        s
    }

    pub fn has(&self, name: &str) -> bool {
        self.binaries.get(name).map(|b| b.available).unwrap_or(false)
    }
}
```

**集成点：**

- `AgentCore::new()` 中调用 `EnvironmentProbe::detect().await`，存储在 `AgentCore` 字段中
- `context.rs` 中的 `build_system_prompt()` 接收 `&EnvironmentProbe`，在 workspace 文件之后插入环境节
- 探测结果也传给 `ErrorClassifier`（见 4.2），用于生成 hint

### 4.2 结构化错误分类（ErrorClassifier）

在工具执行失败后、结果追加到 messages 之前，做一次轻量分类，附加结构化提示。

**新增文件：** `src/tool/error_classifier.rs`

```rust
use crate::agent::probe::EnvironmentProbe;

/// 错误分类枚举
#[derive(Debug, Clone, PartialEq)]
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

/// 分类结果
pub struct ErrorClassification {
    pub kind: ToolErrorKind,
    pub hint: Option<String>,
}

/// 从工具输出中分类错误
pub fn classify_error(
    tool_name: &str,
    output: &str,
    exit_code: Option<i32>,
    env: &EnvironmentProbe,
) -> ErrorClassification {
    // exit code 127 = command not found (Unix)
    // exit code 9009 = command not found (Windows)
    if exit_code == Some(127) || exit_code == Some(9009)
        || output.contains("command not found")
        || output.contains("is not recognized as an internal or external command")
    {
        let command = extract_missing_command(output);
        let hint = suggest_install_command(&command, env);
        return ErrorClassification {
            kind: ToolErrorKind::CommandNotFound { command },
            hint,
        };
    }

    // Python ModuleNotFoundError
    if output.contains("ModuleNotFoundError") || output.contains("No module named") {
        let module = extract_module_name(output);
        let hint = if env.has("pip3") || env.has("pip") {
            let pip = if env.has("uv") { "uv pip install" } else if env.has("pip3") { "pip3 install" } else { "pip install" };
            Some(format!("You may install it: `{pip} {module}`"))
        } else {
            Some("pip is not available. Inform user to install the package.".into())
        };
        return ErrorClassification {
            kind: ToolErrorKind::ModuleNotFound { module, language: "python".into() },
            hint,
        };
    }

    // Node.js module not found
    if output.contains("Cannot find module") || output.contains("MODULE_NOT_FOUND") {
        let module = extract_node_module_name(output);
        let hint = if env.has("npm") {
            Some(format!("You may install it: `npm install {module}`"))
        } else {
            Some("npm is not available.".into())
        };
        return ErrorClassification {
            kind: ToolErrorKind::ModuleNotFound { module, language: "node".into() },
            hint,
        };
    }

    // Permission denied
    if output.contains("Permission denied") || output.contains("EACCES")
        || output.contains("Access is denied")
    {
        return ErrorClassification {
            kind: ToolErrorKind::PermissionDenied,
            hint: Some("Try with appropriate permissions or a different approach.".into()),
        };
    }

    // Syntax error
    if output.contains("SyntaxError") || output.contains("IndentationError") {
        return ErrorClassification {
            kind: ToolErrorKind::SyntaxError { language: "python".into() },
            hint: Some("Check the generated code for syntax issues.".into()),
        };
    }

    // File not found
    if output.contains("No such file or directory") || output.contains("FileNotFoundError")
        || output.contains("The system cannot find the")
    {
        let path = extract_file_path(output);
        return ErrorClassification {
            kind: ToolErrorKind::FileNotFound { path },
            hint: None,
        };
    }

    // Network error
    if output.contains("ConnectionRefusedError") || output.contains("ECONNREFUSED")
        || output.contains("Could not resolve host")
    {
        return ErrorClassification {
            kind: ToolErrorKind::NetworkError,
            hint: Some("Check network connectivity.".into()),
        };
    }

    // Disk full
    if output.contains("No space left on device") || output.contains("ENOSPC") {
        return ErrorClassification {
            kind: ToolErrorKind::DiskFull,
            hint: Some("Disk is full. Free up space before retrying.".into()),
        };
    }

    // Default
    ErrorClassification {
        kind: ToolErrorKind::Unknown,
        hint: None,
    }
}

/// 格式化分类结果，追加到原始 tool_result 输出末尾
pub fn format_error_annotation(classification: &ErrorClassification) -> String {
    let kind_label = match &classification.kind {
        ToolErrorKind::CommandNotFound { command } => format!("command_not_found:{command}"),
        ToolErrorKind::ModuleNotFound { module, language } => format!("module_not_found:{language}:{module}"),
        ToolErrorKind::PermissionDenied => "permission_denied".into(),
        ToolErrorKind::Timeout => "timeout".into(),
        ToolErrorKind::SyntaxError { language } => format!("syntax_error:{language}"),
        ToolErrorKind::NetworkError => "network_error".into(),
        ToolErrorKind::FileNotFound { path } => format!("file_not_found:{path}"),
        ToolErrorKind::DiskFull => "disk_full".into(),
        ToolErrorKind::Unknown => "unknown".into(),
    };

    let mut s = format!("\n\n[error_type: {kind_label}]");
    if let Some(hint) = &classification.hint {
        s += &format!("\n[hint: {hint}]");
    }
    s
}

// ─── 辅助提取函数 ────────────────────────────────────────────────

fn extract_missing_command(output: &str) -> String {
    // 从 "xxx: command not found" 或 "'xxx' is not recognized" 中提取命令名
    // 简单实现：取 "command not found" 前面的 token
    todo!()
}

fn extract_module_name(output: &str) -> String {
    // 从 "No module named 'xxx'" 中提取模块名
    todo!()
}

fn extract_node_module_name(output: &str) -> String {
    // 从 "Cannot find module 'xxx'" 中提取模块名
    todo!()
}

fn extract_file_path(output: &str) -> String {
    // 从错误信息中提取文件路径
    todo!()
}

fn suggest_install_command(command: &str, env: &EnvironmentProbe) -> Option<String> {
    // 根据缺失的命令和环境，建议安装方式
    match command {
        "python3" | "python" => {
            if env.has("uv") {
                Some("uv python install".into())
            } else {
                Some("Install Python from https://python.org or via system package manager.".into())
            }
        }
        "pip3" | "pip" => {
            if env.has("python3") || env.has("python") {
                Some("python3 -m ensurepip --upgrade".into())
            } else {
                Some("Install Python first (pip is included).".into())
            }
        }
        "node" => Some("Install Node.js from https://nodejs.org".into()),
        _ => None,
    }
}
```

**集成点：**

在 `AgentCore::do_handle` 的 tool result 处理逻辑中，对 **shell_exec 的所有结果**调用分类器（不仅限 `is_error: true`）。

> **注意：** 当前 shell_exec 的 exit_code!=0 走的是 `Ok(result_string)`，即 `is_error=false`。
> 只有命令被拦截、超时等 Rust 层 bail 才是 `is_error=true`。
> 因此分类器必须对所有 shell_exec 结果触发，从输出中解析 `[exit code: N]`。

```rust
// 在 execute_batch 返回后
for (call, result) in tool_calls.iter().zip(tool_results.iter_mut()) {
    // 对 shell_exec 的所有结果触发分类（不仅限 is_error）
    // 因为 exit_code!=0 时 shell_exec 返回 Ok（is_error=false）
    let should_classify = result.is_error
        || (call.name == "shell_exec" && result.output.contains("[exit code:") 
            && !result.output.contains("[exit code: 0]"));
    if should_classify {
        let exit_code = parse_exit_code(&result.output); // 从 "[exit code: N]" 解析
        let classification = error_classifier::classify_error(
            &call.name,
            &result.output,
            exit_code,
            &self.env_probe,
        );
        if classification.kind != ToolErrorKind::Unknown {
            result.output += &error_classifier::format_error_annotation(&classification);
        }
    }
}
```

### 4.3 自动安装策略（InstallPolicy）

通过配置控制 LLM 是否可以自主安装依赖。这不是代码层的强制拦截，而是 **prompt 层的策略注入**。

**配置项：** `config.toml`

```toml
[agent]
# 允许 LLM 自主安装 Python 包
auto_install_packages = false

# 安装隔离模式: "venv" = 使用虚拟环境, "user" = --user 安装, "system" = 系统级
install_scope = "venv"

# 虚拟环境路径（install_scope = "venv" 时生效）
venv_path = ".anqclaw/envs"

# 连续错误上限：超过此次数 LLM 应停止重试并汇报
max_consecutive_tool_errors = 3
```

**Prompt 注入逻辑：**

在 `EnvironmentProbe::to_prompt_section()` 中根据配置生成不同指导：

```rust
// auto_install_packages = true 时
s += "### Package Installation Policy\n\n";
s += "- You ARE allowed to install Python packages when needed.\n";
if install_scope == "venv" {
    s += &format!("- ALWAYS use a virtual environment at `{venv_path}`.\n");
    s += "- Create it if it doesn't exist: `python3 -m venv {venv_path}` or `uv venv {venv_path}`.\n";
    s += "- Activate before installing: `source {venv_path}/bin/activate && pip install <pkg>`.\n";
    s += "  On Windows: `{venv_path}\\Scripts\\activate && pip install <pkg>`.\n";
}

// auto_install_packages = false 时
s += "### Package Installation Policy\n\n";
s += "- You are NOT allowed to install packages automatically.\n";
s += "- If a package is missing, inform the user what to install and how.\n";
```

**为什么不在代码层拦截？**

OpenClaw 的经验表明：LLM 自主决策 + 安全护栏比硬编码规则更灵活。`auto_install_packages = false` 时，LLM 通过 prompt 知道不该安装，但 `shell_exec` 本身不拦截 `pip install` 命令。这样：

- 用户口头说"帮我装一下 pandas"时，LLM 可以执行（用户明确授权）
- 默认情况下 LLM 不会自作主张安装
- 不需要维护"哪些命令算安装命令"的规则表

### 4.4 连续错误保护

防止 LLM 在错误循环中反复尝试浪费 token。

**修改文件：** `src/agent/mod.rs`

在 `do_handle` 的 agent loop 中增加连续错误计数：

```rust
let mut consecutive_errors: usize = 0;
let max_consecutive = self.config.agent.max_consecutive_tool_errors; // 默认 3

// 在 tool_results 处理后
// 连续错误计数：per-round 全失败计数
// 即一轮中所有工具都失败时 +1，任何一个成功就重置。
// 注意：不是 per-tool 计数。如果一轮调用 2 个工具、1 成功 1 失败，
// 不会累加计数。这对 anqclaw 场景足够（通常每轮只调一个工具）。
//
// 对 shell_exec 特殊处理：exit_code!=0 也视为失败（即使 is_error=false）
let all_failed = tool_results.iter().all(|r| {
    r.is_error || r.output.contains("[exit code:") && !r.output.contains("[exit code: 0]")
});
if all_failed {
    consecutive_errors += 1;
    if consecutive_errors >= max_consecutive {
        // 注入一条系统提示，告诉 LLM 应该停止重试
        let stop_hint = format!(
            "[system: {} consecutive tool failures detected. \
             Stop retrying and summarize the problem to the user. \
             Suggest manual steps they can take to resolve it.]",
            consecutive_errors
        );
        messages.push(/* system message with stop_hint */);
    }
} else {
    consecutive_errors = 0; // 任何成功都重置计数
}
```

### 4.5 管道命令安全解析

当前 `shell.rs` 只检查命令的第一个 token，无法拦截 `echo hello | rm -rf /`。

**修改文件：** `src/tool/shell.rs`

```rust
/// 拆分 shell 管道和链式命令，返回每个子命令
///
/// 平台差异：
/// - Unix (sh -c):  分隔符为 `|`, `&&`, `||`, `;`
/// - Windows (cmd /C): 分隔符为 `|`, `&&`, `||`, `&`
///   注意：cmd 中 `;` 是参数分隔符而非命令链，`&` 才是无条件链式执行
fn split_command_chain(command: &str) -> Vec<String> {
    // 根据 cfg!(target_os = "windows") 选择分隔符集
    // 按 |, &&, ||, ;/& 分割（注意不拆引号内的字符）
    // 返回每个子命令的 trim 结果
    todo!()
}

/// 检查完整命令链中的每个子命令
fn check_command_chain(&self, command: &str) -> Result<()> {
    let sub_commands = split_command_chain(command);
    for sub in &sub_commands {
        let token = sub.split_whitespace().next().unwrap_or("");
        // 对每个子命令应用相同的安全检查
        if self.is_blocked(token) {
            bail!("blocked command `{token}` in chain: {command}");
        }
        if self.requires_allowlist() && !self.is_allowed(token) {
            bail!("command `{token}` not in allowlist");
        }
    }
    Ok(())
}
```

**集成点：** 替换 `do_execute` 中现有的 `first_token` 检查逻辑。

## 5. System Prompt 重构

当前 `DEFAULT_SYSTEM_PROMPT` 中硬编码了文件处理指令。重构为动态组装：

```
最终 System Prompt 结构：
┌──────────────────────────┐
│ DEFAULT_SYSTEM_PROMPT    │  ← 基础人设 + 通用指南（移除文件处理指令）
├──────────────────────────┤
│ Workspace Files          │  ← SOUL.md / AGENTS.md / TOOLS.md / USER.md
├──────────────────────────┤
│ ## Runtime Environment   │  ← NEW: EnvironmentProbe 动态生成
│   - python3: available   │
│   - pip: available       │
│   - uv: NOT available    │
│   - Guidelines...        │
│   - Install Policy...    │
├──────────────────────────┤
│ ## Available Skills      │  ← 技能摘要（现有逻辑）
├──────────────────────────┤
│ ## Memory                │  ← 记忆搜索结果（现有逻辑）
└──────────────────────────┘
```

**关键变化：**

1. 从 `DEFAULT_SYSTEM_PROMPT` 中**移除**硬编码的 `.docx`/`.xlsx` 处理指令
2. 文件处理指导改为 `EnvironmentProbe::to_prompt_section()` **动态生成**
3. 保留 `pdf_read`、`image_info` 等内建工具的使用说明（这些不依赖外部环境）

## 6. 端到端场景还原

### 场景：用户要求处理 Excel，主机有 Python 但缺 openpyxl

```
用户："帮我汇总 sales.xlsx 的每月销售额"

[System Prompt 含 Runtime Environment 节]
  → python3: available (3.11.5)
  → pip3: available
  → uv: NOT available
  → auto_install_packages: true, scope: venv

LLM 第 1 轮：
  → "Python 可用，我来写个脚本处理"
  → file_write: 写入 process_sales.py
  → shell_exec: "python3 process_sales.py"
  → 失败: "ModuleNotFoundError: No module named 'openpyxl'"

ErrorClassifier 附加：
  → [error_type: module_not_found:python:openpyxl]
  → [hint: You may install it: `pip3 install openpyxl`]

LLM 第 2 轮：
  → "openpyxl 缺失，按安装策略先创建 venv 再安装"
  → shell_exec: "python3 -m venv .anqclaw/envs && .anqclaw/envs/Scripts/activate && pip install openpyxl"
  → 成功

LLM 第 3 轮：
  → shell_exec: ".anqclaw/envs/Scripts/python process_sales.py"
  → 成功，输出 "按月汇总完成，生成 summary.xlsx"

LLM 返回：
  → "已生成 summary.xlsx，共 12 行按月汇总数据。"
```

### 场景：主机无 Python，auto_install 关闭

```
用户："帮我处理 data.xlsx"

[System Prompt 含 Runtime Environment 节]
  → python3: NOT available
  → auto_install_packages: false

LLM 第 1 轮（不试错，直接告知）：
  → "当前系统未安装 Python，处理 Excel 文件需要 Python 环境。
     建议安装步骤：
     1. 访问 https://python.org 下载安装
     2. 安装后运行 pip install openpyxl pandas
     3. 然后再次告诉我，我就可以帮你处理了。"
```

## 7. 实施任务

### Phase 1：环境探测 + Prompt 重构（核心价值，优先实施）

| # | 任务 | 文件 | 改动量 |
|---|------|------|--------|
| 1 | 新增 `EnvironmentProbe` | `src/agent/probe.rs` | ~120 行 |
| 2 | `config.rs` 增加 `auto_install_packages`、`install_scope`、`venv_path`、`max_consecutive_tool_errors` | `src/config.rs` | ~20 行 |
| 3 | `prompt.rs` 移除硬编码文件处理指令，改为接收 `EnvironmentProbe` | `src/agent/prompt.rs` | ~30 行 |
| 4 | `AgentCore` 启动时执行探测，传入 prompt builder | `src/agent/mod.rs` | ~15 行 |
| 5 | 单元测试：环境探测 + prompt 生成 | `src/agent/probe.rs` | ~60 行 |

### Phase 2：结构化错误分类

| # | 任务 | 文件 | 改动量 |
|---|------|------|--------|
| 6 | 新增 `ErrorClassifier` + `parse_exit_code` 辅助函数（从 `[exit code: N]` 解析） | `src/tool/error_classifier.rs` | ~220 行 |
| 7 | `do_handle` 中对 shell_exec 的所有结果（非仅 `is_error`）触发分类器 | `src/agent/mod.rs` | ~20 行 |
| 8 | 单元测试：各类错误的分类和 hint 生成 | `src/tool/error_classifier.rs` | ~100 行 |

> **设计说明：** shell_exec 当前已在输出中包含 `[exit code: N]`（见 shell.rs L222），
> 无需修改 shell.rs 的返回格式。ErrorClassifier 从输出文本中解析 exit_code 即可。

### Phase 3：连续错误保护 + 管道安全

| # | 任务 | 文件 | 改动量 |
|---|------|------|--------|
| 9 | `do_handle` 增加连续错误计数和停止提示（含 exit_code!=0 检测） | `src/agent/mod.rs` | ~25 行 |
| 10 | `shell.rs` 增加 `split_command_chain` + `check_command_chain`（区分 Unix/Windows 分隔符） | `src/tool/shell.rs` | ~80 行 |
| 11 | 单元测试：管道命令解析（含 Windows `&` 分隔）、连续错误保护 | 各文件 | ~80 行 |

## 8. 配置项汇总

```toml
[agent]
# 允许 LLM 自主安装 Python/Node 包（默认关闭）
auto_install_packages = false

# 安装隔离: "venv" = 虚拟环境, "user" = 用户级, "system" = 系统级
install_scope = "venv"

# 虚拟环境目录（相对于 workspace）
venv_path = ".anqclaw/envs"

# 连续工具错误上限，超限后 LLM 应停止重试
max_consecutive_tool_errors = 3

# 启动时探测的额外二进制（追加到默认列表）
probe_extra_binaries = []
```

## 9. 安全考虑

- **安装隔离**：默认使用 venv，不污染系统 Python 环境
- **auto_install 默认关闭**：用户必须主动开启，防止意外安装
- **管道命令解析**：拦截 `echo x | rm -rf /` 类攻击
- **连续错误保护**：防止 LLM 在死循环中无限消耗 token
- **探测超时**：单个 binary 探测超时 2 秒，不影响启动速度
- **不拦截用户明确授权的安装**：`auto_install = false` 是 prompt 层策略，用户口头说"帮我装 pandas"时 LLM 仍可执行

## 10. 未来扩展

- **Docker 沙箱模式**：`sandbox_mode = "docker"` 时 shell_exec 走 `docker exec`
- **环境快照恢复**：安装失败时回滚 venv
- **探测结果缓存**：写入 `.anqclaw/env_cache.json`，避免每次启动重新探测
- **多语言支持**：探测 Go、Rust、Java 等运行时
- **审批协议**：Supervised 模式下对危险命令返回 approval-pending，等待用户确认
