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
    let cjk_tokens = (cjk_chars * 3).div_ceil(2); // ceil(cjk * 1.5)
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
}
