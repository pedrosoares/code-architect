//! Formatting shared by more than one tool — the `[ts] user: text` shape
//! `thread.rs` (a thread's replies) and `channel.rs` (a channel's recent
//! history) both need for one Slack message.

use serde_json::Value;

pub(crate) fn format_message(message: &Value) -> String {
    let user = message["user"].as_str().unwrap_or("unknown");
    let ts = message["ts"].as_str().unwrap_or_default();
    let text = message["text"].as_str().unwrap_or_default();

    format!("[{ts}] {user}: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_one_message() {
        let message = json!({"user": "U123", "ts": "1700000000.000100", "text": "hey, ship it"});

        assert_eq!(
            format_message(&message),
            "[1700000000.000100] U123: hey, ship it"
        );
    }
}
