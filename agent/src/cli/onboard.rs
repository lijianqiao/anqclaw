//! Interactive onboarding wizard for `anqclaw onboard`.
//!
//! Creates `~/.anqclaw/` with config.toml and workspace templates.

use std::io::Write;
use std::path::Path;

use dialoguer::{Input, Select, Confirm};

use crate::paths::{anqclaw_home, ensure_dirs};

// ─── Provider presets ───────────────────────────────────────────────────────

struct ProviderPreset {
    name: &'static str,
    provider: &'static str,
    default_model: &'static str,
    default_base_url: &'static str,
    needs_api_key: bool,
}

const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "Anthropic (Claude)",
        provider: "anthropic",
        default_model: "claude-sonnet-4-20250514",
        default_base_url: "",
        needs_api_key: true,
    },
    ProviderPreset {
        name: "OpenAI (GPT)",
        provider: "openai",
        default_model: "gpt-4o",
        default_base_url: "https://api.openai.com",
        needs_api_key: true,
    },
    ProviderPreset {
        name: "DeepSeek",
        provider: "deepseek",
        default_model: "deepseek-chat",
        default_base_url: "https://api.deepseek.com",
        needs_api_key: true,
    },
    ProviderPreset {
        name: "Qwen (通义千问)",
        provider: "qwen",
        default_model: "qwen-plus",
        default_base_url: "https://dashscope.aliyuncs.com/compatible-mode",
        needs_api_key: true,
    },
    ProviderPreset {
        name: "Gemini",
        provider: "gemini",
        default_model: "gemini-2.5-flash",
        default_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        needs_api_key: true,
    },
    ProviderPreset {
        name: "Ollama (本地模型)",
        provider: "ollama",
        default_model: "qwen2.5:14b",
        default_base_url: "http://localhost:11434",
        needs_api_key: false,
    },
    ProviderPreset {
        name: "MiMo",
        provider: "mimo",
        default_model: "mimo-v2-pro",
        default_base_url: "",
        needs_api_key: true,
    },
    ProviderPreset {
        name: "其他 OpenAI 兼容",
        provider: "openai_compat",
        default_model: "",
        default_base_url: "",
        needs_api_key: true,
    },
];

// ─── Onboard ────────────────────────────────────────────────────────────────

pub fn run_onboard() -> anyhow::Result<()> {
    let home = anqclaw_home();

    println!();
    println!("\x1b[1;36m🚀 Welcome to anqclaw!\x1b[0m");
    println!();

    // Check if config already exists
    let config_path = home.join("config.toml");
    if config_path.exists() {
        let overwrite = Confirm::new()
            .with_prompt(format!(
                "配置文件已存在: {}\n  是否覆盖？",
                config_path.display()
            ))
            .default(false)
            .interact()?;
        if !overwrite {
            println!("取消。已有配置不变。");
            return Ok(());
        }
    }

    // Step 1: Create directory structure
    println!("\x1b[1mStep 1/4:\x1b[0m 创建配置目录");
    ensure_dirs(&home)?;
    println!("  → {} ✓", home.display());
    println!();

    // Step 2: LLM configuration
    println!("\x1b[1mStep 2/4:\x1b[0m LLM 配置");
    let provider_names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
    let selected = Select::new()
        .with_prompt("选择 LLM 提供商")
        .items(&provider_names)
        .default(0)
        .interact()?;

    let preset = &PRESETS[selected];

    let model: String = Input::new()
        .with_prompt("模型名称")
        .default(preset.default_model.to_string())
        .interact_text()?;

    let base_url: String = if preset.default_base_url.is_empty() && preset.provider == "openai_compat" {
        Input::new()
            .with_prompt("Base URL")
            .interact_text()?
    } else {
        Input::new()
            .with_prompt("Base URL")
            .default(preset.default_base_url.to_string())
            .interact_text()?
    };

    let api_key: String = if preset.needs_api_key {
        Input::new()
            .with_prompt("API Key (或环境变量如 ${ANTHROPIC_API_KEY})")
            .interact_text()?
    } else {
        String::new()
    };

    let supports_tools = if preset.provider == "ollama" {
        Confirm::new()
            .with_prompt("该模型是否支持 Tool Calling?")
            .default(false)
            .interact()?
    } else {
        true
    };

    println!();

    // Step 3: Feishu configuration (optional)
    println!("\x1b[1mStep 3/4:\x1b[0m 飞书配置（可选）");
    let setup_feishu = Confirm::new()
        .with_prompt("是否配置飞书?")
        .default(false)
        .interact()?;

    let feishu_config = if setup_feishu {
        let app_id: String = Input::new()
            .with_prompt("App ID")
            .interact_text()?;
        let app_secret: String = Input::new()
            .with_prompt("App Secret (或 ${FEISHU_APP_SECRET})")
            .interact_text()?;
        Some((app_id, app_secret))
    } else {
        None
    };

    println!();

    // Step 4: Generate files
    println!("\x1b[1mStep 4/4:\x1b[0m 生成配置文件");

    // Generate config.toml
    let config_content = generate_config(
        preset.provider,
        &model,
        &base_url,
        &api_key,
        supports_tools,
        feishu_config.as_ref(),
    );
    std::fs::write(&config_path, &config_content)?;
    println!("  → config.toml ✓");

    // Generate workspace templates
    generate_workspace_templates(&home.join("workspace"))?;

    println!();
    println!("\x1b[1;32m✅ 配置完成！\x1b[0m");
    println!();
    println!("  配置文件: {}", config_path.display());
    println!("  工作空间: {}", home.join("workspace").display());
    println!();
    println!("  运行 \x1b[1manqclaw chat\x1b[0m 开始 CLI 对话");
    if feishu_config.is_some() {
        println!("  运行 \x1b[1manqclaw serve\x1b[0m 启动飞书服务");
    }
    println!();

    Ok(())
}

// ─── Config generation ──────────────────────────────────────────────────────

fn generate_config(
    provider: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
    supports_tools: bool,
    feishu: Option<&(String, String)>,
) -> String {
    let mut s = String::new();

    s.push_str("# anqclaw configuration\n");
    s.push_str("# Generated by `anqclaw onboard`\n\n");

    s.push_str("[app]\n");
    s.push_str("name = \"anqclaw\"\n");
    s.push_str("log_level = \"info\"\n\n");

    s.push_str("[llm.default]\n");
    s.push_str(&format!("provider = \"{provider}\"\n"));
    s.push_str(&format!("model = \"{model}\"\n"));
    if !api_key.is_empty() {
        s.push_str(&format!("api_key = \"{api_key}\"\n"));
    }
    if !base_url.is_empty() {
        s.push_str(&format!("base_url = \"{base_url}\"\n"));
    }
    if !supports_tools {
        s.push_str("supports_tools = false\n");
    }
    s.push('\n');

    if let Some((app_id, app_secret)) = feishu {
        s.push_str("[channel.feishu]\n");
        s.push_str(&format!("app_id = \"{app_id}\"\n"));
        s.push_str(&format!("app_secret = \"{app_secret}\"\n"));
        s.push_str("# allow_from = [\"ou_xxxx\"]  # 可选：限制可交互的用户\n\n");
    }

    s.push_str("[tools]\n");
    s.push_str("shell_enabled = true\n");
    s.push_str("shell_permission_level = \"supervised\"\n");
    s.push_str("web_fetch_enabled = true\n");
    s.push_str("file_enabled = true\n");
    s.push_str("memory_tool_enabled = true\n\n");

    s.push_str("[security]\n");
    s.push_str("auto_redact_secrets = true\n");
    s.push_str("# trusted_dirs = [\"~/projects\"]\n");
    s.push_str("# blocked_dirs = []  # System dirs are always blocked\n\n");

    s.push_str("[limits]\n");
    s.push_str("max_requests_per_minute = 20\n");
    s.push_str("max_message_length = 10000\n\n");

    s.push_str("[memory]\n");
    s.push_str("history_limit = 20\n");
    s.push_str("search_limit = 5\n\n");

    s.push_str("[agent]\n");
    s.push_str("max_tool_rounds = 10\n");
    s.push_str("llm_profile = \"default\"\n");
    s.push_str("# fallback_profile = \"deepseek\"  # Fallback LLM when primary fails\n\n");

    s.push_str("[heartbeat]\n");
    s.push_str("enabled = false\n");
    s.push_str("interval_minutes = 30\n\n");

    s.push_str("[audit]\n");
    s.push_str("enabled = false\n");
    s.push_str("log_file = \"logs/audit.jsonl\"\n");
    s.push_str("log_tool_calls = true\n");
    s.push_str("log_shell_commands = true\n");
    s.push_str("log_file_writes = true\n");
    s.push_str("log_llm_calls = false\n");

    s
}

fn generate_workspace_templates(workspace_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(workspace_dir)?;

    let templates: &[(&str, &str)] = &[
        ("SOUL.md", TMPL_SOUL),
        ("AGENTS.md", TMPL_AGENTS),
        ("TOOLS.md", TMPL_TOOLS),
        ("USER.md", TMPL_USER),
        ("MEMORY.md", TMPL_MEMORY),
        ("HEARTBEAT.md", TMPL_HEARTBEAT),
    ];

    for (name, content) in templates {
        let path = workspace_dir.join(name);
        if !path.exists() {
            let mut file = std::fs::File::create(&path)?;
            file.write_all(content.as_bytes())?;
            println!("  → workspace/{name} ✓");
        } else {
            println!("  → workspace/{name} (已存在，跳过)");
        }
    }

    Ok(())
}

// ─── Workspace template content ─────────────────────────────────────────────

const TMPL_SOUL: &str = r#"# 性格设定

你是一个温和、高效、略带幽默感的 AI 助理。

## 语气

- 简洁直接，不啰嗦
- 适当使用口语化表达
- 遇到不确定的事情坦诚说明

## 风格

- 中文优先，必要时使用英文术语
- 代码和技术内容用 Markdown 格式
- 重要信息优先呈现
"#;

const TMPL_AGENTS: &str = r#"# Agent 行为指令

你是用户的私人 AI 助理 anqclaw。

## 规则

- 收到用户消息后认真分析需求，选择合适的工具完成任务
- 如果不确定，先询问用户
- 保持回复简洁有用
- 支持多轮对话，记住上下文
- 使用工具时遵守安全约束

## 决策流程

1. 理解用户意图
2. 判断是否需要使用工具
3. 如需工具，选择最合适的工具执行
4. 整合结果，给出清晰回复
"#;

const TMPL_TOOLS: &str = r#"# 工具使用指南

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
"#;

const TMPL_USER: &str = r#"# 用户画像

<!-- 在此填写用户个人信息和偏好 -->

## 基本信息

- 称呼：
- 时区：Asia/Shanghai

## 偏好

- 语言：中文
- 回复风格：简洁直接
"#;

const TMPL_MEMORY: &str = "# 预置记忆\n\n<!-- 在此填写启动时加载的重要事实，每次构建 system prompt 时会读取 -->\n";

const TMPL_HEARTBEAT: &str = "# Heartbeat\n\n<!-- 定时任务 prompt：每次 heartbeat tick 时读取此文件 -->\n<!-- 如果 LLM 回复包含 \"HEARTBEAT_OK\" 则不通知用户 -->\n<!-- 留空或删除此文件则跳过 heartbeat -->\n";
