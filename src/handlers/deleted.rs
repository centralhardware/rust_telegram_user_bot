use grammers_client::update::MessageDeletion;
use log::info;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;

pub async fn save_deleted(
    deletion: &MessageDeletion,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs() as u32;

    for &msg_id in deletion.messages() {
        // Telegram only names the chat for a channel or a supergroup; for a
        // private chat or a basic group the log has to say where it was.
        let chat_id = match deletion.channel_id() {
            Some(id) => id,
            None => match crate::db::find_private_chat(msg_id as i64).await {
                Some(id) => id,
                None => continue,
            },
        };
        let info = crate::db::find_message(chat_id, msg_id as i64).await;
        let chat_title = if info.chat_title.is_empty() {
            chat_id.to_string()
        } else {
            info.chat_title
        };
        let sender_name = info.first_name;
        let message = info.message;
        let sender_short: String = sender_name.chars().take(10).collect();

        if !is_log_ignored(chat_id) {
            let title_short: String = chat_title.chars().take(25).collect();
            info!(
                "\x1b[91m{:<8} {:>8} {:<25} \x1b[90m│\x1b[91m {:<10} \x1b[90m│\x1b[91m {}\x1b[0m",
                "deleted",
                msg_id,
                title_short,
                sender_short,
                message,
            );
        }

        // Telegram names nothing but the chat and the id, and that is all the
        // row keeps: what the message was is already on its send row.
        crate::db::log_event(Event {
            date_time: now,
            chat_id,
            message_id: msg_id as i64,
            ..Event::delete()
        }).await;
    }

    Ok(())
}
