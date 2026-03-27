use grammers_client::update::Message;
use grammers_tl_types as tl;

/// Format reply line for log messages.
/// Returns a line to print *above* the message, or empty string if no reply.
pub async fn format_reply_line(message: &Message) -> String {
    let reply_id = match message.reply_to_message_id() {
        Some(id) => id,
        None => return String::new(),
    };

    // Extract quote_text from reply header (the highlighted portion the user selected)
    let quote_text = match message.reply_header() {
        Some(tl::enums::MessageReplyHeader::Header(header)) => header.quote_text,
        _ => None,
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    let (text, sender) = lookup_message_text(chat_id, reply_id).await;

    // Place reply_id in the same {:>8} column as the message id in incoming log lines.
    // Layout: {:<8}(9) + {:>8}(9) + {:<25}(26) = 44 before first │
    // Text column starts at 44 + │(2) + {:<10}(11) + │(2) = 59
    let id_col = format!("{:<8} {:>8} {:<25}", "", reply_id, "");
    let pad_text = " ".repeat(59);

    let sender_short: String = match &sender {
        Some(name) if !name.is_empty() => name.chars().take(10).collect(),
        _ => String::new(),
    };

    match text {
        Some(text) if !text.is_empty() => {
            // If there's a quote, highlight that portion within the full text
            let highlighted = if let Some(ref qt) = quote_text {
                highlight_quote(&text, qt)
            } else {
                text
            };
            let formatted = highlighted.lines().enumerate().map(|(i, line)| {
                if i == 0 {
                    format!("{id_col}\x1b[90m│ {:<10} │ > {line}", sender_short)
                } else {
                    format!("{pad_text}\x1b[90m    {line}")
                }
            }).collect::<Vec<_>>().join("\n");
            format!("{formatted}\x1b[0m")
        }
        _ => format!("{id_col}\x1b[90m│ {:<10} │ > [{reply_id}]\x1b[0m", sender_short),
    }
}

/// Highlight the quote portion within the full text using cyan color
fn highlight_quote(text: &str, quote: &str) -> String {
    match text.find(quote) {
        Some(pos) => {
            let before = &text[..pos];
            let after = &text[pos + quote.len()..];
            format!("{before}\x1b[96m{quote}\x1b[90m{after}")
        }
        None => text.to_string(),
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
