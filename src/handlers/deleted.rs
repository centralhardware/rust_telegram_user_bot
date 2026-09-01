use grammers_client::update::MessageDeletion;
use log::info;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;

pub async fn save_deleted(
    deletion: &MessageDeletion,
) -> Result<(), Box<dyn std::error::Error>> {
    let channel_id = match deletion.channel_id() {
        Some(id) => id,
        None => return Ok(()),
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs() as u32;

    for &msg_id in deletion.messages() {
        if !is_log_ignored(channel_id) {
            info!(
                "\x1b[91m{:<8} {:>8} {}\x1b[0m",
                "deleted", msg_id, channel_id,
            );
        }

        // Telegram names nothing but the chat and the id, and that is all the
        // row keeps: the message it points at is already in the log.
        crate::db::EVENTS_BUF.push(Event {
            date_time: now,
            chat_id: channel_id,
            message_id: msg_id as i64,
            ..Event::delete()
        }).await;
    }

    Ok(())
}
