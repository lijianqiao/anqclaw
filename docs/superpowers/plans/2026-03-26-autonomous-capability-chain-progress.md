# anqclaw — 自主能力链实施进度

> 实施计划: `docs/superpowers/plans/2026-03-26-autonomous-capability-chain-plan.md`
> 设计规格书: `docs/autonomous-capability-chain-design.md`

---

## 总览

| 阶段 | 内容 | 状态 | 完成时间 | 备注 |
|------|------|------|----------|------|
| Phase 1 | 环境探测 + Prompt 重构 | - [x] 已完成 | 2026-03-26 | 76+5 单元/集成测试全过 |
| Phase 2 | 结构化错误分类 | - [x] 已完成 | 2026-03-26 | 16 分类测试 + 全部 162 测试通过 |
| Phase 3 | 连续错误保护 + 管道安全 | - [x] 已完成 | 2026-03-26 | 10 新测试 + 全部 214 测试通过 |

---

## Phase 1: 环境探测 + Prompt 重构

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 1.1 | config.rs — AgentSection 新增配置项 | - [x] 已完成 | 5 字段: auto_install_packages, install_scope, venv_path, max_consecutive_tool_errors, probe_extra_binaries |
| 1.2 | 新增 probe.rs — EnvironmentProbe | - [x] 已完成 | ~230 行, detect()+to_prompt_section()+has(), Windows python3 虚拟映射 |
| 1.3 | prompt.rs — 移除硬编码 File Handling | - [x] 已完成 | 移除 .docx/.xlsx 硬编码 python3 命令 |
| 1.4 | context.rs — build_system_prompt 接收 EnvironmentProbe | - [x] 已完成 | 所有 3 个 prompt 构建路径均注入 env_section |
| 1.5 | agent/mod.rs — AgentCore 集成 EnvironmentProbe | - [x] 已完成 | new() 改 async, detect() 在启动时调用 |
| 1.6 | 单元测试 — EnvironmentProbe | - [x] 已完成 | 7 个测试全过 |

**阶段完成标志:** `cargo check --all-targets` 通过 + probe 测试通过 + system prompt 包含 Runtime Environment 节

---

## Phase 2: 结构化错误分类

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 2.1 | 新增 error_classifier.rs | - [x] 已完成 | ~310 行, 9 ErrorKind 变体, classify_error()+format_error_annotation()+parse_exit_code(), 16 测试 |
| 2.2 | agent/mod.rs — 集成 ErrorClassifier | - [x] 已完成 | results 改 mut, audit 之后 messages.push 之前插入分类循环, Unknown 不注解 |
| 2.3 | 单元测试 — ErrorClassifier | - [x] 已完成 | 16 测试全过: command_not_found(unix/win), module_not_found(py/node/submodule), permission, syntax, file_not_found, network, disk_full, unknown, parse_exit_code, format, hint_uv, hint_no_pip |

**阶段完成标志:** `cargo check --all-targets` 通过 + 16 分类测试通过 + tool result 末尾含 error_type 注解 ✅

---

## Phase 3: 连续错误保护 + 管道安全

| Task | 内容 | 状态 | 备注 |
|------|------|------|------|
| 3.1 | agent/mod.rs — 连续错误保护 | - [x] 已完成 | consecutive_errors 计数器, all_failed 判定(含 exit_code!=0), max_consecutive 触发 stop hint 注入 |
| 3.2 | shell.rs — 管道命令解析 | - [x] 已完成 | split_command_chain (|/&&/||/;/&), check_command_chain 替换原 first_token 检查, 引号保护 |
| 3.3 | 单元测试 | - [x] 已完成 | 8 管道测试 + 2 连续错误测试, 全部 214 测试通过 |

**阶段完成标志:** `cargo test` 全部通过 + 管道安全 8 测试 + 连续错误保护 2 测试 ✅
