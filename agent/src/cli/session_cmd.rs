//! `anqclaw sessions`, `anqclaw export`, `anqclaw import` implementations.

use std::path::Path;

use anyhow::{Context, Result};

use crate::memory::MemoryStore;

/// `anqclaw sessions` — list all sessions.
pub async fn run_list(memory: &MemoryStore) -> Result<()> {
    let sessions = memory.list_sessions().await?;

    if sessions.is_empty() {
        println!("No sessions found. / 未找到任何会话。");
        return Ok(());
    }

    println!("{:<40} {:>6} {:>20}", "CHAT_ID", "MSGS", "LAST ACTIVE");
    println!("{}", "-".repeat(68));

    for s in &sessions {
        let dt = chrono::DateTime::from_timestamp(s.last_active, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| s.last_active.to_string());
        println!("{:<40} {:>6} {:>20}", s.chat_id, s.message_count, dt);
    }

    println!(
        "\nTotal: {} session(s) / 共 {} 个会话",
        sessions.len(),
        sessions.len()
    );
    Ok(())
}

/// `anqclaw sessions clean --before <duration>` — delete old sessions.
///
/// `before` is a human duration string like "30d", "7d", "24h".
pub async fn run_clean(memory: &MemoryStore, before: &str) -> Result<()> {
    let seconds = parse_duration(before)?;
    let cutoff = chrono::Utc::now().timestamp() - seconds;

    let deleted = memory.delete_sessions_before(cutoff).await?;
    let dt = chrono::DateTime::from_timestamp(cutoff, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| cutoff.to_string());

    println!(
        "Deleted {deleted} message(s) from sessions last active before {dt}. / 已删除 {dt} 之前最后活跃的会话共 {deleted} 条消息。"
    );
    Ok(())
}

/// `anqclaw sessions delete <chat_id>` — delete a specific session.
pub async fn run_delete(memory: &MemoryStore, chat_id: &str) -> Result<()> {
    let deleted = memory.delete_session(chat_id).await?;

    if deleted == 0 {
        println!("No messages found for session '{chat_id}'. / 会话 '{chat_id}' 未找到任何消息。");
    } else {
        println!(
            "Deleted {deleted} message(s) from session '{chat_id}'. / 已从会话 '{chat_id}' 删除 {deleted} 条消息。"
        );
    }
    Ok(())
}

/// `anqclaw export <chat_id> [-o file.json]` — export a session to JSON.
pub async fn run_export(memory: &MemoryStore, chat_id: &str, output: Option<&str>) -> Result<()> {
    let export = memory.export_session(chat_id).await?;

    if export.messages.is_empty() {
        anyhow::bail!(
            "session '{chat_id}' not found or has no messages / 会话 '{chat_id}' 未找到或无消息"
        );
    }

    let json =
        serde_json::to_string_pretty(&export).context("serialize session / 序列化会话失败")?;

    if let Some(path) = output {
        tokio::fs::write(path, &json)
            .await
            .with_context(|| format!("write to {path} / 写入 {path} 失败"))?;
        println!(
            "Exported {} message(s) → {path} / 已导出 {} 条消息至 {path}",
            export.messages.len(),
            export.messages.len()
        );
    } else {
        // Default filename: <chat_id>.json
        let filename = format!("{chat_id}.json");
        tokio::fs::write(&filename, &json)
            .await
            .with_context(|| format!("write to {filename} / 写入 {filename} 失败"))?;
        println!(
            "Exported {} message(s) → {filename} / 已导出 {} 条消息至 {filename}",
            export.messages.len(),
            export.messages.len()
        );
    }

    Ok(())
}

/// `anqclaw import <file.json>` — import a session from JSON.
pub async fn run_import(memory: &MemoryStore, file: &str) -> Result<()> {
    let path = Path::new(file);
    anyhow::ensure!(path.exists(), "file not found / 文件未找到: {file}");

    let json = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {file} / 读取 {file} 失败"))?;

    let export: crate::memory::SessionExport =
        serde_json::from_str(&json).context("parse session JSON / 解析会话 JSON 失败")?;

    let count = export.messages.len();
    let chat_id = export.chat_id.clone();

    memory.import_session(&export).await?;

    println!(
        "Imported {count} message(s) into session '{chat_id}'. / 已将 {count} 条消息导入会话 '{chat_id}'。"
    );
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse a human-friendly duration string into seconds.
/// Supports: "30d" (days), "24h" (hours), "60m" (minutes), "3600s" (seconds).
fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    anyhow::ensure!(
        !s.is_empty(),
        "duration string cannot be empty / 时间字符串不能为空"
    );

    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: i64 = num_str
        .parse()
        .with_context(|| format!("invalid duration number / 无效的时间数字: '{num_str}'"))?;

    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => anyhow::bail!(
            "unknown duration unit '{unit}', expected one of: s, m, h, d / 未知的时间单位 '{unit}'，支持: s, m, h, d"
        ),
    };

    Ok(num * multiplier)
}
