use std::sync::LazyLock;

static IGNORED_CHAT_IDS: LazyLock<Vec<i64>> = LazyLock::new(|| {
    std::env::var("LOG_IGNORE_CHATS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .collect()
});

pub fn is_log_ignored(chat_id: i64) -> bool {
    IGNORED_CHAT_IDS.contains(&chat_id)
}

/// Check if a log message mentions an ignored chat via `Channel(ID)` or `Chat(ID)`.
pub fn is_message_ignored(msg: &str) -> bool {
    if IGNORED_CHAT_IDS.is_empty() {
        return false;
    }
    for keyword in ["Channel(", "Chat("] {
        if let Some(start) = msg.find(keyword) {
            let after = &msg[start + keyword.len()..];
            if let Some(end) = after.find(')') {
                if let Ok(id) = after[..end].parse::<i64>() {
                    if IGNORED_CHAT_IDS.contains(&id) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
