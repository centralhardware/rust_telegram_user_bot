use grammers_client::update::Message;

/// Format reply line for log messages.
/// Returns `"\n\x1b[90m> reply text\x1b[0m"` suffix if reply found, empty string otherwise.
pub async fn format_reply_line(message: &Message, limit: usize) -> String {
    let reply_id = match message.reply_to_message_id() {
        Some(id) => id,
        None => return String::new(),
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    let text = lookup_message_text(chat_id, reply_id).await;

    match text {
        Some(text) if !text.is_empty() => {
            let preview: String = text.chars().take(limit).collect();
            let ellipsis = if text.chars().count() > limit { "…" } else { "" };
            let full = format!("{preview}{ellipsis}");
            let formatted = full.lines().enumerate().map(|(i, line)| {
                if i == 0 {
                    format!("\x1b[90m> {line}")
                } else {
                    format!("\x1b[90m  {line}")
                }
            }).collect::<Vec<_>>().join("\n");
            format!("\n{formatted}\x1b[0m")
        }
        _ => format!("\n\x1b[90m> [{reply_id}]\x1b[0m"),
    }
}

async fn lookup_message_text(chat_id: i64, message_id: i32) -> Option<String> {
    // Check unflushed incoming buffer first
    let from_buf = crate::db::INCOMING_BUF.find_last(|m| {
        if m.chat_id == chat_id && m.message_id == message_id as i64 {
            Some(m.message.clone())
        } else {
            None
        }
    }).await;
    if from_buf.is_some() {
        return from_buf;
    }

    // Query ClickHouse: try chats_log (incoming), then telegram_messages_new (outgoing)
    let db = crate::db::clickhouse();

    if let Ok(text) = db
        .query("SELECT message FROM chats_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1")
        .bind(chat_id)
        .bind(message_id as i64)
        .fetch_one::<String>()
        .await
    {
        return Some(text);
    }

    if let Ok(text) = db
        .query("SELECT message FROM telegram_messages_new WHERE id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1")
        .bind(chat_id)
        .bind(message_id as u64)
        .fetch_one::<String>()
        .await
    {
        return Some(text);
    }

    None
}
