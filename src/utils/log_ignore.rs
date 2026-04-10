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
