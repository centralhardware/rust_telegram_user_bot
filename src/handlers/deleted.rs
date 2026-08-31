use grammers_client::update::MessageDeletion;
use log::info;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;

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

    for &msg_id in deletion.messages() {
        let info = crate::db::find_message(channel_id, msg_id as i64).await;
        let chat_title = if info.chat_title.is_empty() {
            channel_id.to_string()
        } else {
            info.chat_title
        };
        let sender_name = info.first_name;
        let message = info.message;
        let sender_short: String = sender_name.chars().take(10).collect();

        if !is_log_ignored(channel_id) {
            let title_short: String = chat_title.chars().take(25).collect();
            info!(
                "\x1b[91m{:<8} {:>8} {:<25} \x1b[90m│\x1b[91m {:<10} \x1b[90m│\x1b[91m {}\x1b[0m",
                "deleted",
                msg_id,
                title_short,
                sender_short,
                &message,
            );
        }

        // Telegram names nothing but the id, so the row keeps what the message
        // was: on its own a delete row would say only that something vanished.
        crate::db::EVENTS_BUF.push(Event {
            date_time: now,
            chat_id: channel_id,
            chat_title,
            message_id: msg_id as i64,
            message,
            first_name: sender_name,
            client_id,
            ..Event::delete()
        }).await;
    }

    Ok(())
}
