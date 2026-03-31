# anqclaw

[中文版本](README_zh.md)

anqclaw is a personal AI assistant built in Rust. It currently supports Feishu, HTTP, and CLI entry points, with multi-LLM collaboration, tool calling, persistent memory, and runtime bootstrapping for real-world tasks.

## Current Capabilities

- Multiple LLM profiles: Anthropic, OpenAI-compatible, Ollama, and more
- Agentic loop with tool calling and streaming responses
- Multi-channel access: Feishu, HTTP API, and CLI
- Skills mainline: candidate skills are exposed as structured `<available_skills>`, and the model reads `SKILL.md` on demand
- Built-in tools: shell, web, file, memory, pdf_read, image_info, and custom tools
- SQLite conversation history and long-term memory, with a source table plus FTS5 index mirror
- Python task bootstrap: prepares a workspace `.venv` and runs scripts when needed
- Default safety controls: supervised shell, file sandboxing, SSRF checks, and audit logging

## Architecture Overview

Main flow:

Feishu/HTTP/CLI Channel -> Gateway -> AgentCore -> ToolRegistry/MemoryStore -> Channel

Core modules:

- `channel`: Feishu, HTTP, and CLI input/output
- `gateway`: routing, deduplication, rate limiting, and per-session serialization
- `agent`: context assembly, environment probing, and the agentic loop
- `llm`: provider abstraction and client implementations
- `tool`: tool registration and execution
- `skill`: multi-source skill scanning, candidate summaries, and hot reload
- `memory`: SQLite-backed history and long-term memory
- `audit` / `metrics` / `scheduler`: auditing, metrics, and background tasks

## Skills Mainline

- Skill packages use the directory form: `skills/<name>/SKILL.md`
- Skill sources are merged in `bundled -> user(~/.anqclaw/skills) -> workspace(<workspace>/skills_dir)` order, with later sources overriding earlier ones
- The agent uses `description` as an automatic candidate-matching signal, then refines ranking with `keywords`, `trigger`, `extensions`, recent file tokens, and workspace extensions before injecting readable locations through structured `<available_skills>`
- When a skill is relevant, the primary path is for the model to read the corresponding `SKILL.md` through `file_read`; `activate_skill` remains only as a compatibility or debugging path
- In `serve` mode, skill directories are hot-reloaded and the triggering file paths are logged for auditability

## Deployment

Requirements:

- A matching release binary for your OS and CPU architecture
- A valid config file
- Read/write access for the app directory and data directory
- Network access to your LLM provider and channel integrations

### Windows

Recommended path:

```text
C:\anqclaw\anqclaw.exe
```

Optional: add `C:\anqclaw\` to `PATH`.

With `PATH`:

```powershell
anqclaw.exe onboard
anqclaw.exe config validate
anqclaw.exe serve
```

Without `PATH`:

```powershell
C:\anqclaw\anqclaw.exe onboard
C:\anqclaw\anqclaw.exe config validate
C:\anqclaw\anqclaw.exe serve
```

As needed:

- Install Microsoft Visual C++ Redistributable
- Install Python or `uv` if you enable Python bootstrap or package installation flows
- Install any external commands required by your prompts or custom tools

### Linux

Recommended path:

```text
/opt/anqclaw/anqclaw
```

Prepare:

```bash
chmod +x /opt/anqclaw/anqclaw
ln -sf /opt/anqclaw/anqclaw /usr/local/bin/anqclaw
```

With `PATH`:

```bash
anqclaw onboard
anqclaw config validate
anqclaw serve
```

Without `PATH`:

```bash
/opt/anqclaw/anqclaw onboard
/opt/anqclaw/anqclaw config validate
/opt/anqclaw/anqclaw serve
```

As needed:

- Install Python or `uv` if you enable Python bootstrap or package installation flows
- Install any external commands required by your prompts or custom tools

### macOS

Recommended path:

```text
/usr/local/anqclaw/anqclaw
```

Prepare:

```bash
chmod +x /usr/local/anqclaw/anqclaw
ln -sf /usr/local/anqclaw/anqclaw /usr/local/bin/anqclaw
```

With `PATH`:

```bash
anqclaw onboard
anqclaw config validate
anqclaw serve
```

Without `PATH`:

```bash
/usr/local/anqclaw/anqclaw onboard
/usr/local/anqclaw/anqclaw config validate
/usr/local/anqclaw/anqclaw serve
```

As needed:

- If the first launch is blocked, run `xattr -d com.apple.quarantine /usr/local/anqclaw/anqclaw`
- Install Python or `uv` if you enable Python bootstrap or package installation flows
- Install any external commands required by your prompts or custom tools

## Development

Use this section only if you are changing code or debugging locally.

Requirements:

- `rustup`, `rustc`, `cargo`
- Platform build tools
  - Windows: Visual Studio Build Tools / MSVC
  - Linux: `gcc` or `clang`

Common commands:

```bash
cd agent
cargo build
cargo run -- onboard
cargo run -- chat
cargo run -- serve
cargo run -- config validate
```

## Build a Release Binary

Requirement: Rust toolchain installed.

Build:

```bash
cd agent
cargo build --release
```

Output:

- Windows: `agent/target/release/anqclaw.exe`
- Linux/macOS: `agent/target/release/anqclaw`

## Quality Status

- Local validation has passed with `cargo test --manifest-path agent/Cargo.toml`
- Local validation also includes `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo audit` is used as a local dependency check; some remaining advisories are currently inherited from upstream transitive dependencies
- Recent regression coverage includes custom tools, trusted path handling, web SSRF, interrupted streams, Feishu token refresh, concurrent long-term memory writes, and the skills candidate-selection plus on-demand-read mainline

## Docs

- Autonomous capability chain design: [docs/autonomous-capability-chain-design.md](docs/autonomous-capability-chain-design.md)
- Baseline architecture design: [docs/2026-03-24-anqclaw-v1-design.md](docs/2026-03-24-anqclaw-v1-design.md)
- File extraction design: [docs/2026-03-26-file-extraction-design.md](docs/2026-03-26-file-extraction-design.md)
