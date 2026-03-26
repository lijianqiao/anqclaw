# anqclaw — 二进制文件提取设计方案

> Created: 2026-03-26
> Reference: ZeroClaw `file_read.rs`, `pdf_read.rs`, `image_info.rs`

---

## 1. 问题

当前 `file_read` 工具使用 `tokio::fs::read_to_string`，遇到二进制文件（PDF、DOCX、XLSX、图片）直接报 I/O 错误，LLM 无法自动恢复。用户体验差。

## 2. 设计目标

- 用户发送任何文件，助手都能尝试提取可读内容
- 纯 Rust 方案优先，不强依赖 Python 环境
- 无法原生处理的格式，通过 system prompt 引导 LLM 用 shell 自行解决
- 使用 feature gate 控制可选的重量级依赖

## 3. ZeroClaw 参考分析

| 工具 | 策略 | 技术 |
|------|------|------|
| `file_read` | 智能降级链：`read_to_string` → 检测 PDF 头自动提取 → `from_utf8_lossy` | `pdf_extract` crate |
| `pdf_read` | 专用工具，`spawn_blocking` 跑 CPU 密集提取，支持 `max_chars` 截断 | feature gate `rag-pdf` |
| `image_info` | 魔数识别格式 + 头部解析宽高 + 可选 base64 输出 | 纯 Rust 手写，无外部 crate |
| DOCX/XLSX | 不支持，留给 shell 兜底 | — |

**关键决策：** ZeroClaw 不走 Python 子进程，全部纯 Rust。DOCX/XLSX 完全不支持。

## 4. anqclaw 方案

采用 **三层策略**：Rust 原生工具 + file_read 降级链 + system prompt 兜底。

### 4.1 修改 `file_read` — 智能降级链

当前行为：`read_to_string` 失败 → 返回错误。

改为：

```
文件大小预检 → 超过 FILE_READ_MAX_SIZE (20MB) 直接返回元信息摘要
       ↓ 通过
read_to_string 成功 → 带行号返回（现有逻辑）
       ↓ 失败
read 原始字节（受大小限制保护）
       ↓
检测 %PDF- 头 → 调用 pdf_extract（如果启用）
       ↓ 不是 PDF
检测图片魔数 → 返回格式/尺寸元信息 + 提示使用 image_info
       ↓ 不是图片
检测已知二进制格式魔数（ZIP/PK、EXE/MZ、SQLite、gzip 等）
  → 返回 "[二进制文件: {format}, {size} bytes, 无法直接读取]"
       ↓ 都不是
String::from_utf8_lossy 降级输出，截断到前 2000 字符 + 警告
```

**关键改进：**
- 最前面加文件大小预检，防止大文件打爆内存
- 新增已知二进制格式检测层，避免对 .exe/.zip/.sqlite 等文件产出无意义乱码
- lossy 路径增加 2000 字符截断，防止 token 浪费

**改动文件：** `src/tool/file.rs`

**改动量：** ~60 行

### 4.2 新增 `pdf_read` 工具

独立工具，专门处理 PDF 提取，支持 `max_chars` 参数控制输出长度。

**参数 schema：**

```json
{
  "path": "string (required) — PDF 文件路径",
  "max_chars": "integer (optional, default: 50000, max: 200000) — 最大返回字符数"
}
```

**实现：**

- 使用 `pdf_extract` crate，通过 feature gate `rag-pdf` 控制
- `spawn_blocking` 避免阻塞 async runtime
- 未启用 feature 时工具仍注册，但返回清晰错误提示
- 文件大小上限 50MB

**改动文件：** 新增 `src/tool/pdf_read.rs`，修改 `src/tool/mod.rs`

**依赖：**

```toml
# Cargo.toml
[dependencies]
pdf_extract = { version = "0.7", optional = true }

[features]
default = []
rag-pdf = ["pdf_extract"]
```

### 4.3 新增 `image_info` 工具

读取图片元信息，纯 Rust 实现，无外部依赖。

**参数 schema：**

```json
{
  "path": "string (required) — 图片文件路径",
  "include_base64": "boolean (optional, default: false) — 是否返回 base64 编码数据"
}
```

**实现：**

- 魔数检测格式：PNG (`\x89PNG`)、JPEG (`\xFF\xD8\xFF`)、GIF (`GIF8`)、WebP (`RIFF...WEBP`)、BMP (`BM`)
- 头部字节解析宽高（PNG IHDR、JPEG SOF、GIF header、BMP header）
- 可选 base64 输出（供多模态 LLM 使用），**硬上限 1MB**：超过 1MB 的图片跳过 base64 编码并返回提示，因为 1MB 图片 base64 后约 1.37MB 文本，对大多数 LLM context window 已经很大
- 文件大小上限 10MB

**改动文件：** 新增 `src/tool/image_info.rs`，修改 `src/tool/mod.rs`

**无新增依赖**（`base64` crate 已在项目中）

### 4.4 DOCX / XLSX — System Prompt 兜底

不新增专用工具。在 system prompt 中增加引导规则：

```
当用户请求读取 .docx 文件时，使用 shell_exec 执行 Python：
  python3 -c "from docx import Document; d=Document('path'); print('\n'.join(p.text for p in d.paragraphs))"

当用户请求读取 .xlsx 文件时，使用 shell_exec 执行 Python：
  python3 -c "import openpyxl; wb=openpyxl.load_workbook('path'); ..."
```

**条件：** 用户机器上需安装 `python-docx`、`openpyxl`。若不可用，LLM 应告知用户安装。

### 4.5 配置项

在 `config.toml` 的 `[tools]` section 新增：

```toml
# PDF extraction (requires rag-pdf feature)
pdf_read_enabled = true
pdf_read_max_chars = 50000

# Image info
image_info_enabled = true
```

## 5. 实施任务

### Phase 1: file_read 降级链 + pdf_read

| # | 任务 | 文件 | 依赖 |
|---|------|------|------|
| 1 | Cargo.toml 添加 `pdf_extract` optional 依赖 + `rag-pdf` feature | `Cargo.toml` | — |
| 2 | 新增 `pdf_read.rs` | `src/tool/pdf_read.rs` | #1 |
| 3 | 修改 `file_read` 降级链：大小预检 → UTF-8 → PDF → 图片 → 二进制魔数 → lossy(截断) | `src/tool/file.rs` | #1 |
| 4 | `mod.rs` 注册 `pdf_read` 工具 | `src/tool/mod.rs` | #2 |
| 5 | `config.rs` 添加 `pdf_read_enabled` 等配置项 | `src/config.rs` | — |
| 6 | 单元测试 | `src/tool/pdf_read.rs` | #2 |

### Phase 2: image_info

| # | 任务 | 文件 | 依赖 |
|---|------|------|------|
| 7 | 新增 `image_info.rs` — 魔数检测 + 宽高解析 + base64 | `src/tool/image_info.rs` | — |
| 8 | `mod.rs` 注册 `image_info` 工具 | `src/tool/mod.rs` | #7 |
| 9 | `config.rs` 添加 `image_info_enabled` 配置项 | `src/config.rs` | — |
| 10 | 单元测试：每种格式的魔数检测 + 宽高解析 | `src/tool/image_info.rs` | #7 |

### Phase 3: System Prompt 引导

| # | 任务 | 文件 | 依赖 |
|---|------|------|------|
| 11 | system prompt 追加 DOCX/XLSX 处理引导规则 | `src/agent/prompt.rs` | — |
| 12 | 集成测试：验证 PDF、图片、二进制文件的完整降级链 | `tests/integration_test.rs` | #4, #8 |

## 6. 文件结构变更

```
src/tool/
├── file.rs          # 修改：降级链
├── pdf_read.rs      # 新增：PDF 专用工具
├── image_info.rs    # 新增：图片元信息工具
├── mod.rs           # 修改：注册新工具
├── shell.rs
├── web.rs
├── memory_tool.rs
├── model_tool.rs
├── skill_tool.rs
└── custom.rs
```

## 7. 安全考虑

- **路径校验**：所有新工具复用现有 `resolve_safe_path` + `check_blocked_dirs` 逻辑
- **文件大小限制**：file_read 20MB 预检、PDF 50MB、图片 10MB，防止内存炸毁
- **CPU 保护**：PDF 提取用 `spawn_blocking`，不阻塞 async executor
- **输出截断**：`max_chars` 防止返回给 LLM 的文本过大；file_read lossy 路径截断到 2000 字符
- **二进制文件保护**：file_read 检测已知二进制格式魔数（ZIP/EXE/SQLite/gzip），返回元信息而非乱码
- **base64 输出安全**：image_info 的 base64 仅用于多模态 LLM，不含可执行内容；硬上限 1MB 防止 context window 溢出

## 8. 未来扩展

- **DOCX/XLSX 原生支持**：待 Rust 生态成熟后可加 `docx-rs`、`calamine` 等 crate
- **OCR**：图片文字提取可接 Tesseract 或多模态 LLM vision API
- **多模态消息**：当 LLM provider 支持 vision 时，image_info 的 base64 可直接作为消息内容发送
