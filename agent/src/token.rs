//! Lightweight token estimation and context window management.
//!
//! Uses a simple heuristic: ~4 characters per token for English/code,
//! ~2 characters per token for CJK. No external tokenizer dependency.

/// Estimate the number of tokens in a string.
///
/// Heuristic: count ASCII words (÷0.75) + CJK characters (×1.5) + other.
/// This intentionally over-estimates slightly to stay within limits.
pub fn estimate_tokens(text: &str) -> usize {
    let mut ascii_chars = 0usize;
    let mut cjk_chars = 0usize;

    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_chars += 1;
        } else if is_cjk(ch) {
            cjk_chars += 1;
        } else {
            // Other Unicode (emoji etc.) — count as ~1.5 tokens each
            cjk_chars += 1;
        }
    }

    // English: ~4 chars per token; CJK: ~1.5 chars per token
    let ascii_tokens = ascii_chars / 4;
    let cjk_tokens = (cjk_chars * 3 + 1) / 2; // ceil(cjk * 1.5)
    ascii_tokens + cjk_tokens + 1 // +1 for safety margin
}

fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compat Ideographs
        | '\u{3000}'..='\u{303F}' // CJK Symbols
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul
    )
}

/// Estimate tokens for a ChatMessage (role label + content).
pub fn estimate_message_tokens(role: &str, content: &str) -> usize {
    // Each message has ~4 tokens overhead (role, formatting)
    4 + estimate_tokens(role) + estimate_tokens(content)
}

/// Trim history messages to fit within a token budget.
///
/// Keeps the system prompt(s) and the most recent messages.
/// Returns the index into `messages` from which to start including.
///
/// `messages` layout: [system, ...system_extras, ...history, user_msg]
/// We always keep system messages (at the start) and the last user message.
/// History is trimmed from the oldest.
#[allow(dead_code)]
pub fn trim_history_to_budget(
    messages: &[(String, String)], // (role, content) pairs
    max_tokens: u64,
) -> usize {
    if max_tokens == 0 {
        return 0; // no limit
    }

    // Calculate total tokens
    let token_counts: Vec<usize> = messages
        .iter()
        .map(|(role, content)| estimate_message_tokens(role, content))
        .collect();

    let total: usize = token_counts.iter().sum();
    if total as u64 <= max_tokens {
        return 0; // everything fits
    }

    // Find where system messages end
    let system_end = messages
        .iter()
        .position(|(role, _)| role != "system")
        .unwrap_or(messages.len());

    // System tokens (always kept)
    let system_tokens: usize = token_counts[..system_end].iter().sum();
    // Last message tokens (always kept — it's the current user message)
    let last_tokens = *token_counts.last().unwrap_or(&0);

    let budget_for_history = max_tokens.saturating_sub(system_tokens as u64 + last_tokens as u64);

    // Scan history from newest to oldest, accumulating until budget exhausted
    let history_range = system_end..messages.len().saturating_sub(1);
    let mut accumulated = 0u64;
    let mut keep_from = history_range.start;

    for i in history_range.clone().rev() {
        let msg_tokens = token_counts[i] as u64;
        if accumulated + msg_tokens > budget_for_history {
            keep_from = i + 1;
            break;
        }
        accumulated += msg_tokens;
        keep_from = i;
    }

    keep_from
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_english() {
        let text = "Hello world, this is a test sentence.";
        let tokens = estimate_tokens(text);
        // ~36 ASCII chars → ~9 tokens + 1 margin = ~10
        assert!(tokens > 5 && tokens < 20, "got {tokens}");
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        let text = "你好世界这是测试";
        let tokens = estimate_tokens(text);
        // 8 CJK chars → ~12 tokens + 1 = ~13
        assert!(tokens > 8 && tokens < 20, "got {tokens}");
    }

    #[test]
    fn test_trim_all_fits() {
        let messages = vec![
            ("system".into(), "You are helpful.".into()),
            ("user".into(), "Hi".into()),
        ];
        assert_eq!(trim_history_to_budget(&messages, 1000), 0);
    }

    #[test]
    fn test_trim_exceeds_budget() {
        let long_msg = "a".repeat(4000); // ~1000 tokens
        let messages = vec![
            ("system".into(), "prompt".into()),
            ("user".into(), long_msg.clone()),
            ("assistant".into(), long_msg.clone()),
            ("user".into(), long_msg.clone()),
            ("assistant".into(), long_msg.clone()),
            ("user".into(), "current question".into()),
        ];
        // Budget of 2500 tokens — should drop some history
        let start = trim_history_to_budget(&messages, 2500);
        assert!(start > 1, "expected trimming, got start={start}");
    }
}
