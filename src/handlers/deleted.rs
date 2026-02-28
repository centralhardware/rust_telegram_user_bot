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
        let chat_title = ch
            .query(
                "SELECT chat_title FROM chats_log WHERE chat_id = ? ORDER BY date_time DESC LIMIT 1",
            )
            .bind(channel_id)
            .fetch_one::<String>()
            .await
            .unwrap_or_else(|_| channel_id.to_string());

        let message = ch
            .query(
                "SELECT message FROM edited_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1",
            )
            .bind(channel_id)
            .bind(msg_id as i64)
            .fetch_one::<String>()
            .await
            .or_else(|_| {
                // We can't do async in or_else, so we'll handle this below
                Err(())
            });

        let message = match message {
            Ok(m) => m,
            Err(_) => ch
                .query(
                    "SELECT message FROM chats_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1",
                )
                .bind(channel_id)
                .bind(msg_id as i64)
                .fetch_one::<String>()
                .await
                .unwrap_or_else(|_| msg_id.to_string()),
        };

        let title_short: String = chat_title.chars().take(25).collect();
        info!(
            "\x1b[91m{:<15} {:>5} {:<25} {}\x1b[0m",
            "deleted",
            msg_id,
            title_short,
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
