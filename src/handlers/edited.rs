use grammers_client::update::Message;
use log::info;

use crate::db::EditedMessage;

pub async fn save_edited(
    message: &Message,
    client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let chat_id = message.peer_id().bare_id_unchecked();
    let msg_id = message.id() as i64;
    let message_content = message.text().to_string();

    if message_content.is_empty() {
        return Ok(());
    }

    // Check buffers first — data may not be flushed to DB yet
    let original = if let Some(msg) = crate::db::EDITED_BUF
        .find_last(|e| {
            (e.chat_id == chat_id && e.message_id == msg_id).then(|| e.message.clone())
        })
        .await
    {
        msg
    } else {
        let ch = crate::db::clickhouse();
        let db_result = ch
            .query(
                "SELECT message FROM (\
                    SELECT message, 1 AS p, date_time FROM edited_log WHERE chat_id = ? AND message_id = ? \
                    UNION ALL \
                    SELECT message, 2 AS p, date_time FROM chats_log WHERE chat_id = ? AND message_id = ? \
                ) ORDER BY p, date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(msg_id)
            .bind(chat_id)
            .bind(msg_id)
            .fetch_one::<String>()
            .await
            .unwrap_or_default();

        if db_result.is_empty() {
            // Message might still be in the incoming buffer
            crate::db::INCOMING_BUF
                .find_last(|m| {
                    (m.chat_id == chat_id && m.message_id == msg_id).then(|| m.message.clone())
                })
                .await
                .unwrap_or_default()
        } else {
            db_result
        }
    };

    if original.is_empty() || original == message_content {
        return Ok(());
    }

    let diff = unified_diff(&original, &message_content);

    let user_id = message
        .sender()
        .and_then(|s| s.id().bare_id())
        .unwrap_or(0) as i64;

    let chat_name = message
        .peer()
        .map(|p| p.name().unwrap_or_default().to_string())
        .unwrap_or_default();

    let chat_name_short: String = chat_name.chars().take(25).collect();
    info!(
        "\x1b[93m{:<15} {:>5} {:<25}\n{}\x1b[0m",
        "edited",
        message.id(),
        chat_name_short,
        diff,
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as u32;

    crate::db::EDITED_BUF.push(EditedMessage {
        date_time: now,
        chat_id,
        message_id: msg_id,
        original_message: original,
        message: message_content,
        diff,
        user_id,
        client_id,
    }).await;

    Ok(())
}

fn unified_diff(original: &str, modified: &str) -> String {
    similar::TextDiff::from_lines(original, modified)
        .unified_diff()
        .to_string()
}
