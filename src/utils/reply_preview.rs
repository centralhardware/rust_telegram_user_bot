use grammers_client::update::Message;

/// Format reply line for log messages.
/// Returns a line to print *above* the message, or empty string if no reply.
pub async fn format_reply_line(message: &Message) -> String {
    let reply_id = match message.reply_to_message_id() {
        Some(id) => id,
        None => return String::new(),
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    let (text, sender) = lookup_message_text(chat_id, reply_id).await;

    // Align with message text: {:<8}(9) + {:>8}(9) + {:<25}(26) + │(2) + {:<10}(11) + │(2) = 59
    // Logger already adds [HH:MM:SS] prefix since this is a separate info!() call
    let pad = " ".repeat(59);

    let sender_prefix = match &sender {
        Some(name) if !name.is_empty() => format!("{name}: "),
        _ => String::new(),
    };

    match text {
        Some(text) if !text.is_empty() => {
            let formatted = text.lines().enumerate().map(|(i, line)| {
                if i == 0 {
                    format!("{pad}\x1b[90m> {sender_prefix}{line}")
                } else {
                    format!("{pad}\x1b[90m  {line}")
                }
            }).collect::<Vec<_>>().join("\n");
            format!("{formatted}\x1b[0m")
        }
        _ => format!("{pad}\x1b[90m> {sender_prefix}[{reply_id}]\x1b[0m"),
    }
}

async fn lookup_message_text(chat_id: i64, message_id: i32) -> (Option<String>, Option<String>) {
    // Check unflushed incoming buffer first
    let from_buf = crate::db::INCOMING_BUF.find_last(|m| {
        if m.chat_id == chat_id && m.message_id == message_id as i64 {
            let sender = if m.second_name.is_empty() {
                m.first_name.clone()
            } else {
                format!("{} {}", m.first_name, m.second_name)
            };
            Some((m.message.clone(), sender))
        } else {
            None
        }
    }).await;
    if let Some((text, sender)) = from_buf {
        return (Some(text), Some(sender));
    }

    // Query ClickHouse: try chats_log (incoming), then telegram_messages_new (outgoing)
    let db = crate::db::clickhouse();

    if let Ok((text, first, second)) = db
        .query("SELECT message, first_name, second_name FROM chats_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1")
        .bind(chat_id)
        .bind(message_id as i64)
        .fetch_one::<(String, String, String)>()
        .await
    {
        let sender = if second.is_empty() { first } else { format!("{first} {second}") };
        return (Some(text), Some(sender));
    }

    if let Ok(text) = db
        .query("SELECT message FROM telegram_messages_new WHERE id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1")
        .bind(chat_id)
        .bind(message_id as u64)
        .fetch_one::<String>()
        .await
    {
        return (Some(text), None);
    }

    (None, None)
}
