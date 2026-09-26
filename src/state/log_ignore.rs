//! Chats kept out of the console, from `LOG_IGNORE_CHATS`. They are still
//! logged to the database; only the console lines are skipped.

#[derive(Default)]
pub struct LogIgnore(Vec<i64>);

impl LogIgnore {
    pub fn from_env() -> Self {
        LogIgnore(
            std::env::var("LOG_IGNORE_CHATS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| s.trim().parse::<i64>().ok())
                .collect(),
        )
    }

    pub fn contains(&self, chat_id: i64) -> bool {
        self.0.contains(&chat_id)
    }

    /// Check if a log message mentions an ignored chat via `Channel(ID)` or `Chat(ID)`.
    pub fn is_message_ignored(&self, msg: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        for keyword in ["Channel(", "Chat("] {
            if let Some(start) = msg.find(keyword) {
                let after = &msg[start + keyword.len()..];
                if let Some(end) = after.find(')')
                    && let Ok(id) = after[..end].parse::<i64>()
                    && self.contains(id)
                {
                    return true;
                }
            }
        }
        false
    }
}
