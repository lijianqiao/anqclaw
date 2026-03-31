//! @file
//! @author <lijianqiao>
//! @since <2026-03-31>
//! @brief 负责统一会话键的生成规则，避免各入口重复实现 session_key_strategy。

/// Build a session key from the configured strategy.
pub fn build_session_key(strategy: &str, chat_id: &str, sender_id: &str) -> String {
    match strategy {
        "user" => sender_id.to_string(),
        "chat_user" => format!("{chat_id}::{sender_id}"),
        _ => chat_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::build_session_key;

    #[test]
    fn test_build_session_key_defaults_to_chat() {
        assert_eq!(build_session_key("chat", "chat_a", "user_a"), "chat_a");
        assert_eq!(build_session_key("unknown", "chat_a", "user_a"), "chat_a");
    }

    #[test]
    fn test_build_session_key_supports_user_strategy() {
        assert_eq!(build_session_key("user", "chat_a", "user_a"), "user_a");
    }

    #[test]
    fn test_build_session_key_supports_chat_user_strategy() {
        assert_eq!(
            build_session_key("chat_user", "chat_a", "user_a"),
            "chat_a::user_a"
        );
    }
}
