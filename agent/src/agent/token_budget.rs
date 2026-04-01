//! @file
//! @author <lijianqiao>
//! @since <2026-03-31>
//! @brief 负责对 agent 会话消息执行 token 预算裁剪并复用已缓存估算值。

use crate::types::{ChatMessage, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrimOutcome {
    pub removed_messages: usize,
    pub total_tokens: usize,
    pub budget: u64,
    pub trimmed_all_history: bool,
}

pub(super) fn trim_messages_to_budget(
    messages: &mut Vec<ChatMessage>,
    token_budget: u64,
) -> Option<TrimOutcome> {
    if token_budget == 0 || messages.len() <= 1 {
        return None;
    }

    let total_tokens: usize = messages
        .iter_mut()
        .map(ChatMessage::estimate_tokens_cached)
        .sum();
    if total_tokens as u64 <= token_budget {
        return None;
    }

    let system_end = messages
        .iter()
        .position(|message| !matches!(message.role, Role::System))
        .unwrap_or(messages.len());
    let history_end = messages.len().saturating_sub(1);
    if history_end <= system_end {
        return None;
    }

    let system_tokens: usize = messages[..system_end]
        .iter()
        .filter_map(ChatMessage::estimated_tokens)
        .sum();
    let last_tokens = messages
        .last()
        .and_then(ChatMessage::estimated_tokens)
        .unwrap_or(0) as u64;
    let budget_for_history = token_budget.saturating_sub(system_tokens as u64 + last_tokens);

    if budget_for_history == 0 {
        let removed_messages = history_end - system_end;
        if removed_messages == 0 {
            return None;
        }
        messages.drain(system_end..history_end);
        return Some(TrimOutcome {
            removed_messages,
            total_tokens,
            budget: token_budget,
            trimmed_all_history: true,
        });
    }

    let mut accumulated = 0u64;
    let mut keep_from = system_end;
    for index in (system_end..history_end).rev() {
        let tokens = messages[index].estimate_tokens_cached() as u64;
        if accumulated + tokens > budget_for_history {
            keep_from = index + 1;
            break;
        }
        accumulated += tokens;
        keep_from = index;
    }

    let removed_messages = keep_from.saturating_sub(system_end);
    if removed_messages == 0 {
        return None;
    }

    messages.drain(system_end..keep_from);
    Some(TrimOutcome {
        removed_messages,
        total_tokens,
        budget: token_budget,
        trimmed_all_history: false,
    })
}
