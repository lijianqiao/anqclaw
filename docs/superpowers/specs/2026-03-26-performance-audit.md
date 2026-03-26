# anqclaw 性能与代码逻辑审计报告

**日期**: 2026-03-26  
**范围**: `agent/src/` 全部 30+ 源文件  
**类型**: 性能 · 算法 · 并发 · 逻辑正确性

---

## 🔴 CRITICAL（必须修复）

### C1. `gateway.rs` — `rate_limiter` DashMap 条目无限增长

**位置**: gateway.rs:37, 155-168  
**问题**: `rate_limiter: DashMap<String, Vec<Instant>>` 为每个唯一 session_key 创建条目，永不清理。`retain()` 只清理 Vec 内的旧时间戳，但空 Vec 的条目本身永远不会从 DashMap 中移除。

```rust
// 当前：只清理 Vec 内部
entry.retain(|t| now.duration_since(*t) < window);
// 但空 Vec 永远留在 DashMap 中 → 内存泄漏
```

**影响**: 长期运行后内存持续增长。若有大量不同 chat_id（HTTP API 场景），增长更快。  
**修复方案**: 在 `retain` 之后判断空 Vec 并移除条目；或定期在 Gateway 主循环中添加 GC 逻辑：

```rust
// 方案 A：每次检查后清理空条目
entry.retain(|t| now.duration_since(*t) < window);
if entry.is_empty() {
    drop(entry); // 先释放 RefMut
    self.rate_limiter.remove(&session_key);
}

// 方案 B：定期 GC（推荐，减少锁竞争）
// 在 Gateway::run() 的 main loop 旁添加一个 interval task
```

---

### C2. `gateway.rs` — `chat_locks` DashMap 条目无限增长

**位置**: gateway.rs:34  
**问题**: 与 C1 同类。`chat_locks: DashMap<String, Arc<Mutex<()>>>` 为每个 session_key 创建锁条目，永不清理。  
**影响**: 每个对话创建一个 `Arc<Mutex>` 永久留在内存。  
**修复方案**: 在 `_guard` 释放后、若无其他引用（`Arc::strong_count == 1`）则移除条目。

---

### C3. `channel/http.rs` — `rate_limiter` DashMap 与 C1 同样问题

**位置**: http.rs:145, 227-240 (handle_chat)  
**问题**: HTTP channel 自己维护的 `rate_limiter: DashMap<String, Vec<Instant>>` 同样不清理空条目。  
**影响**: 同 C1。  
**修复方案**: 同 C1。

---

### C4. `channel/http.rs` — `pending` HashMap 条目泄漏

**位置**: http.rs:42, 268-282  
**问题**: 当客户端断开连接时（HTTP 连接中断），`pending` 中注册的 `oneshot::Sender` 不会被清理。只有在 Gateway 的 `send_message` 被调用时才删除。如果 Gateway 没有响应（宕机/超时），条目永久残留。  
**影响**: 内存泄漏 + oneshot::Sender 持有引用。  
**修复方案**: 在 `handle_chat` 的 timeout 分支中已有清理逻辑 ✅。但如果客户端在 5 分钟 timeout 之前断开 TCP 连接，axum 的 handler future 被 cancel，`pending` 不会清理。用 `Drop` guard 包装。

---

## 🟠 HIGH（发布前应修复）

### H1. `gateway.rs` — 速率限制的 check+insert 非原子

**位置**: gateway.rs:155-168  
**问题**: `entry.retain()` → `if entry.len() >= max_rpm` → `entry.push(now)` 不是一个原子操作。DashMap 的 `entry()` 持有写锁期间是安全的，但是 `or_default()` 返回的 `RefMut` 在整个代码块结束前持有锁。

**验证结果**: 仔细审查后，DashMap `entry().or_default()` 返回 `RefMut`，其作用域覆盖了 retain + len check + push，所以**这里实际是原子的**。✅ 无问题。

---

### H2. `agent/mod.rs` — token 预算计算重复遍历

**位置**: agent/mod.rs:147-193  
**问题**: `estimate_message_tokens` 被调用两次遍历 messages：
1. 第一次 `total_tokens: usize = messages.iter().map(|m| estimate_message_tokens(...)).sum()` — O(n)
2. 如果超预算，再次对 system messages 和历史扫描 — 又 O(n)

每次 LLM 请求都遍历所有消息两次。且 `estimate_message_tokens` 内部按字符遍历。  
**影响**: 双重遍历，在长对话中（如 100+ 条历史）有不必要的 CPU 消耗。  
**修复方案**: 合为一次遍历，同时计算 total、system_tokens、各消息 token 数：

```rust
let mut token_counts: Vec<usize> = messages.iter()
    .map(|m| token::estimate_message_tokens("msg", &m.content))
    .collect();
let total_tokens: usize = token_counts.iter().sum();
// 后续操作使用 token_counts[i] 而非重复计算
```

---

### H3. `tool/web.rs` — `strip_html_tags` 不过滤 `<script>` / `<style>` 内容

**位置**: web.rs:136-148  
**问题**: 简单的 `in_tag` 状态机只移除 `<>` 标签本身，但 `<script>...code...</script>` 和 `<style>...css...</style>` 中间的 **内容** 会被保留。这些内容对 LLM 毫无用处，且可能包含 XSS payload。

```
输入:  <script>alert('XSS')</script><p>Hello</p>
当前结果: alert('XSS')Hello
期望结果: Hello
```

**影响**: LLM 上下文被无用的 JS/CSS 代码污染，浪费 token。  
**修复方案**: 在 `strip_html_tags` 中增加 `in_script`/`in_style` 二级状态，跳过这些标签的内容。

---

### H4. `channel/feishu/ws.rs` — `seen_ids` HashMap 无上限

**位置**: ws.rs:53, 260-268  
**问题**: `seen_ids: HashMap<String, Instant>` 的 GC 策略是 `retain(30min)`，但如果 30 分钟内收到大量唯一消息，HashMap 会膨胀。无容量上限。  
**影响**: 理论上可以被大量消息 ID 轰炸导致内存膨胀（实际风险低，飞书场景下消息速率有限）。  
**修复方案**: 改用 `LruCache` 限制最大条目数（如 10000），同时保留时间窗口清理。

---

### H5. `channel/feishu/ws.rs` — Fragment cache 无大小上限

**位置**: ws.rs:128  
**问题**: `frag_cache: HashMap<String, FragEntry>` 只有 5 分钟的时间 GC，但无条目数量上限。恶意分片消息可以填充该缓存。  
**影响**: 低风险（飞书协议管理分片数量）但不够健壮。  
**修复方案**: 添加 `if frag_cache.len() > 100 { frag_cache.clear(); }` 上限保护。

---

### H6. `llm/anthropic.rs` & `openai_compat.rs` — SSE buffer 无上限

**位置**: anthropic.rs:234, openai_compat.rs:163  
**问题**: 流式解析中的 `buffer: String` 持续累积 chunk 数据：

```rust
buffer.push_str(&String::from_utf8_lossy(&chunk));
```

已处理的行从 buffer 头部截断，但通过 `buffer = buffer[pos + 1..].to_string()` 每次创建新 String 副本。  
**影响**: 
1. 每次截断都分配新 String（O(n) 复制）
2. 如果服务端发送异常大 chunk 而无换行符，buffer 无限增长  
**修复方案**:
- 用 `drain(..pos+1)` 或 `VecDeque<u8>` 替代 String 截断，避免重复分配
- 添加 buffer 大小上限（如 1MB），超过则中断流

---

### H7. `tool/mod.rs` — `definitions()` 每次调用重建 Vec

**位置**: tool/mod.rs:160-168  
**问题**: `definitions()` 每次被调用时遍历所有 tools 并克隆 name、description、parameters_schema。这在 agentic loop 的每一轮都被调用。  
**影响**: 小开销但可避免。在 max_tool_rounds=20 时，重复 20 次。  
**修复方案**: 在 `ToolRegistry::new()` 中预构建 `Vec<ToolDefinition>` 并缓存：

```rust
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    definitions_cache: Vec<ToolDefinition>,  // 构造时填充
}
```

---

### H8. `skill.rs` — `std::sync::RwLock` 在 async 上下文中使用

**位置**: skill.rs:38-40, 68-76  
**问题**: `SkillRegistry` 使用 `std::sync::RwLock`，在 `list()` 和 `find()` 中调用 `read()`。这些方法在 async 代码路径（如 `ToolRegistry::new()`, `AgentCore::do_handle()`）中被调用。

**严重程度评估**: `std::sync::RwLock::read()` 本身是非阻塞的（如果锁可用则立即返回）。问题在于：
- 如果 `reload()` 的 `write()` 正好在持有锁时，`read()` 会**阻塞当前 tokio 线程**
- `reload()` 内部执行 `scan_skills()`，涉及文件 I/O（`read_dir` + `read_to_string`），持锁时间可能较长

**影响**: 在 skill 热重载时可能短暂阻塞 tokio worker thread。  
**修复方案**: 
- 方案 A: `reload()` 先在锁外执行 `scan_skills()`（当前已是如此 ✅），仅在赋值时持有 write lock → 持锁时间极短，风险很低  
- 方案 B: 改用 `tokio::sync::RwLock` → 写锁不阻塞 runtime  
- 现状评估：**当前代码已在锁外完成 I/O，锁内只做 Vec swap**，所以实际风险低。保留观察。

---

## 🟡 MEDIUM（建议修复）

### M1. `gateway.rs` — `channels.iter().find()` 线性查找

**位置**: gateway.rs:172, 191, 199  
**问题**: 每次处理消息时查找目标 channel 用 `iter().find(|c| c.name() == msg.channel)`。当 channel 数量 >3 时效率低。  
**影响**: 极小（通常只有 2-3 个 channel），但代码重复。  
**修复方案**: 用 `HashMap<String, Arc<dyn Channel>>` 预索引 channel 名称。

---

### M2. `agent/mod.rs` — `msg.images.clone()` 深拷贝 base64 数据

**位置**: agent/mod.rs:137  
**问题**: `let image_data: Vec<ImageData> = msg.images.clone()` 深拷贝所有图片的 base64 字符串。一张 1MB 图片的 base64 约 1.3MB。  
**影响**: 多图场景下额外内存分配显著。  
**修复方案**: `ChatMessage` 的 `images` 字段改用 `Arc<Vec<ImageData>>` 或 `Cow` 共享所有权。

---

### M3. `agent/redact.rs` — 多次 `String::replace` 链式调用

**位置**: redact.rs:52-60, 66-86  
**问题**: 对每个 secret、每个 BUILTIN_PATTERN、每个 extra_pattern 分别执行 `result.replace()` 或手工扫描。每次 `replace()` 创建新 String。

若有 N 个 secret + M 个 pattern，复杂度为 O((N+M) × len)。  
**影响**: 在 secret 数量少时（通常 <5）可忽略。  
**修复方案**: 暂不优化，当 secret 数量增长时可改用 Aho-Corasick 多模式匹配（一次扫描完成所有替换）。

---

### M4. `agent/context.rs` — 每次请求读取文件系统

**位置**: context.rs:33-58  
**问题**: `build_system_prompt()` 每次 agent 处理消息时尝试读取 5 个工作区文件（SOUL.md, AGENTS.md 等）。文件 I/O 在 agentic loop 中不应每轮执行。  
**影响**: 文件系统调用开销（通常有 OS 缓存，实际影响小）。如果文件在网络挂载上则影响大。  
**修复方案**: 
- 在 `AgentCore::new()` 中构建 system prompt 并缓存
- 结合 skill_summary 动态拼接（skill_summary 可能变化但频率低）
- 或添加 TTL 缓存，如 5 分钟刷新一次

---

### M5. `memory/mod.rs` — `search_memory` 的 FTS5 查询未转义

**位置**: memory/mod.rs:228-240  
**问题**: 用户输入直接传入 FTS5 MATCH 语句。FTS5 有自己的查询语法（AND, OR, NOT, 引号等）。特殊字符如 `*`, `"`, `-` 可能导致查询异常或返回意外结果。

```rust
.bind(query)  // 用户原始输入直接作为 MATCH 表达式
```

**影响**: 非安全漏洞（SQLx 参数绑定防止了 SQL 注入），但可能导致 FTS5 语法错误返回零结果。  
**修复方案**: 在绑定前对 query 进行 FTS5 转义，将特殊字符用双引号包裹：

```rust
let escaped = format!("\"{}\"", query.replace('"', "\"\""));
```

---

### M6. `memory/mod.rs` — `get_history` 查询效率

**位置**: memory/mod.rs:153-175  
**问题**: `ORDER BY created_at DESC, id DESC LIMIT ?` 后在 Rust 端 `messages.reverse()`。这在功能上正确，但：
1. 反转发生在内存中，创建额外 Vec 操作
2. 如果 (chat_id, created_at, id) 无复合索引，大表时查询慢

**影响**: 当前规模下可忽略。  
**修复方案**: 在 `schema.sql` 中添加 `CREATE INDEX IF NOT EXISTS idx_messages_chat_time ON messages(chat_id, created_at, id)` 索引。

---

### M7. `llm/anthropic.rs` & `openai_compat.rs` — reqwest Client 未复用

**位置**: anthropic.rs:38, openai_compat.rs:39  
**问题**: 每次创建 `AnthropicClient` / `OpenAiCompatClient` 都 `Client::builder().build()`。如果切换 model profile，每次创建新 reqwest Client（包括新的连接池）。  
**影响**: 连接池未复用，TLS 握手重复。  
**修复方案**: 传入共享的 `reqwest::Client` 而非每次构建。低优先级（profile 切换频率低）。

---

### M8. `tool/custom.rs` — 输出无大小限制

**问题**: 自定义工具执行外部命令，stdout/stderr 读入内存无大小限制。命令输出 1GB 数据会耗尽内存。  
**修复方案**: `read_to_end` 改为限定大小读取，如最多 1MB。

---

## 🟢 LOW（改善代码质量）

### L1. `types.rs` — `MessageContent::to_text()` 克隆 Text 内容

**位置**: types.rs:74  
**问题**: `MessageContent::Text(s) => s.clone()` 返回 `String`。在 Gateway 中多次调用（消息长度检查 + agent 处理），每次克隆完整内容。  
**修复方案**: 改为 `fn to_text(&self) -> &str`，返回引用。

---

### L2. `agent/mod.rs` — `tool_defs` 在循环外构建但不变

**位置**: agent/mod.rs:195  
**问题**: `let tool_defs = self.tools.definitions()` 在每次 `do_handle` 调用时重建。跨 agentic loop 的多轮中不变。  
**现状**: 当前已在循环外构建（正确） ✅。但 `definitions()` 本身每次新建 Vec（见 H7）。

---

### L3. Cargo.toml — tokio "full" feature

**位置**: Cargo.toml:7  
**问题**: `features = ["full"]` 启用了所有 tokio 特性。实际只需 `rt-multi-thread`, `macros`, `net`, `io-util`, `time`, `process`, `signal`, `sync`, `fs`。  
**影响**: 编译时间略长，二进制体积略大。  
**修复方案**: 按需启用特性（低优先级）。

---

## 📊 汇总

| 级别 | 数量 | 关键发现 |
|------|------|----------|
| 🔴 CRITICAL | 4 | DashMap/HashMap 内存泄漏 (C1-C4) |
| 🟠 HIGH | 8 | token 重复计算、HTML 过滤不全、SSE buffer 无限长 |
| 🟡 MEDIUM | 8 | 线性查找、深拷贝图片、FTS5 未转义 |
| 🟢 LOW | 3 | 不必要克隆、cargo features 过宽 |

**注意**: 子agent 报告的 "edition = 2024 无效" 为误报 — Rust 1.85+ (2025-02 稳定) 确实支持 edition 2024；"shell 命令注入" 在 Full 模式下是设计允许的（Full 即完全信任）；"std::sync::Mutex 在 async 中使用" 经验证 gateway.rs 用的是 `tokio::sync::Mutex` ✅。
