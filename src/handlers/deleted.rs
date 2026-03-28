use grammers_client::update::MessageDeletion;
use log::info;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::DeletedMessage;

pub async fn save_deleted(
    deletion: &MessageDeletion,
    client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let channel_id = match deletion.channel_id() {
        Some(id) => id,
        None => return Ok(()),
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs() as u32;

    let ch = crate::db::clickhouse();

    for &msg_id in deletion.messages() {
        let mid = msg_id as i64;

        // Check buffers first — data may not be flushed to DB yet
        let (buf_chat_title, buf_sender, buf_message) = {
            let from_edited = crate::db::EDITED_BUF
                .find_last(|e| {
                    (e.chat_id == channel_id && e.message_id == mid).then(|| e.message.clone())
                })
                .await;

            let from_incoming = crate::db::INCOMING_BUF
                .find_last(|m| {
                    (m.chat_id == channel_id && m.message_id == mid)
                        .then(|| (m.chat_title.clone(), m.first_name.clone(), m.message.clone()))
                })
                .await;

            match from_incoming {
                Some((title, sender, msg)) => (
                    Some(title),
                    Some(sender),
                    from_edited.or(Some(msg)),
                ),
                None => (None, None, from_edited),
            }
        };

        let chat_title = if let Some(t) = buf_chat_title.filter(|t| !t.is_empty()) {
            t
        } else {
            ch.query("SELECT chat_title FROM chats_log WHERE chat_id = ? ORDER BY date_time DESC LIMIT 1")
                .bind(channel_id)
                .fetch_one::<String>()
                .await
                .unwrap_or_else(|_| channel_id.to_string())
        };

        let sender_name = if let Some(s) = buf_sender.filter(|s| !s.is_empty()) {
            s
        } else {
            ch.query("SELECT first_name FROM chats_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1")
                .bind(channel_id)
                .bind(mid)
                .fetch_one::<String>()
                .await
                .unwrap_or_default()
        };

        let message = if let Some(m) = buf_message.filter(|m| !m.is_empty()) {
            m
        } else {
            ch.query(
                    "SELECT message FROM (\
                        SELECT message, 1 AS p, date_time FROM edited_log WHERE chat_id = ? AND message_id = ? \
                        UNION ALL \
                        SELECT message, 2 AS p, date_time FROM chats_log WHERE chat_id = ? AND message_id = ?\
                    ) ORDER BY p, date_time DESC LIMIT 1",
                )
                .bind(channel_id)
                .bind(mid)
                .bind(channel_id)
                .bind(mid)
                .fetch_one::<String>()
                .await
                .unwrap_or_default()
        };
        let sender_short: String = sender_name.chars().take(10).collect();

        let title_short: String = chat_title.chars().take(25).collect();
        info!(
            "\x1b[91m{:<8} {:>8} {:<25} \x1b[90m│\x1b[91m {:<10} \x1b[90m│\x1b[91m {}\x1b[0m",
            "deleted",
            msg_id,
            title_short,
            sender_short,
            message,
        );

        crate::db::DELETED_BUF.push(DeletedMessage {
            date_time: now,
            chat_id: channel_id,
            message_id: msg_id as i64,
            client_id,
        }).await;
    }

    Ok(())
}
