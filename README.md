# anqclaw

[中文版本](README_zh.md)

anqclaw is a personal AI assistant built in Rust. It currently supports Feishu, HTTP, and CLI entry points, with multi-LLM collaboration, tool calling, persistent memory, and runtime bootstrapping for real-world tasks.

## Current Capabilities

- Multiple LLM profiles: Anthropic, OpenAI-compatible, Ollama, and more
- Agentic loop with tool calling and streaming responses
- Multi-channel access: Feishu, HTTP API, and CLI
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
- `memory`: SQLite-backed history and long-term memory
- `audit` / `metrics` / `scheduler`: auditing, metrics, and background tasks

## Quick Start

1. Build

```bash
cd agent
cargo build
```

2. Initial setup

```bash
cargo run -- onboard
```

3. CLI chat

```bash
cargo run -- chat "hello"
# or interactive mode
cargo run -- chat
```

4. Start the service

```bash
cargo run -- serve
```

5. Show or validate configuration

```bash
cargo run -- config show
cargo run -- config validate
```

## Quality Status

- Local validation has passed with `cargo test --manifest-path agent/Cargo.toml`
- CI is in place for tests, clippy, and cargo-audit
- Recent regression coverage includes custom tools, trusted path handling, web SSRF, interrupted streams, Feishu token refresh, and concurrent long-term memory writes

## Docs

- Autonomous capability chain design: [docs/autonomous-capability-chain-design.md](docs/autonomous-capability-chain-design.md)
- Baseline architecture design: [docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md](docs/superpowers/specs/2026-03-24-anqclaw-v1-design.md)
- File extraction design: [docs/superpowers/specs/2026-03-26-file-extraction-design.md](docs/superpowers/specs/2026-03-26-file-extraction-design.md)
