use clickhouse::Row;
use grammers_client::update::MessageDeletion;
use log::info;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::schedulers;

#[derive(Row, Serialize)]
struct DeletedMessage {
    date_time: u32,
    chat_id: i64,
    message_id: i64,
    client_id: u64,
}

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

    let clickhouse_client = schedulers::clickhouse_client()?;

    for &msg_id in deletion.messages() {
        let chat_title = clickhouse_client
            .query(
                "SELECT chat_title FROM chats_log WHERE chat_id = ? ORDER BY date_time DESC LIMIT 1",
            )
            .bind(channel_id)
            .fetch_one::<String>()
            .await
            .unwrap_or_else(|_| channel_id.to_string());

        let message = clickhouse_client
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
            Err(_) => clickhouse_client
                .query(
                    "SELECT message FROM chats_log WHERE chat_id = ? AND message_id = ? ORDER BY date_time DESC LIMIT 1",
                )
                .bind(channel_id)
                .bind(msg_id as i64)
                .fetch_one::<String>()
                .await
                .unwrap_or_else(|_| msg_id.to_string()),
        };

        let preview: String = message.chars().take(80).collect();
        let title_short: String = chat_title.chars().take(25).collect();
        info!(
            "\x1b[91m{:<15} {:>5} {:<25} {}\x1b[0m",
            "deleted",
            msg_id,
            title_short,
            preview,
        );

        let mut insert = clickhouse_client
            .insert::<DeletedMessage>("deleted_log")
            .await?;
        insert
            .write(&DeletedMessage {
                date_time: now,
                chat_id: channel_id,
                message_id: msg_id as i64,
                client_id,
            })
            .await?;
        insert.end().await?;
    }

    Ok(())
}
