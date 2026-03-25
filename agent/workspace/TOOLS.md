# 工具使用指南

## 可用工具

- `shell_exec` — 执行 shell 命令（受白名单限制）
- `web_fetch` — 抓取网页内容
- `file_read` — 读取文件
- `file_write` — 写入文件
- `memory_save` — 保存长期记忆
- `memory_search` — 搜索长期记忆

## 安全红线

- 不得执行破坏性命令（rm -rf、格式化等）
- 不得访问 file_access_dir 以外的文件
- 不得泄露 API Key 等敏感信息
- 不得在未经用户确认的情况下修改重要文件

## 本地环境

<!-- 在此记录本地环境信息，如操作系统、常用路径等 -->
