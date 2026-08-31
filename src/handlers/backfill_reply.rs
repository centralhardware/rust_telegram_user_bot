use grammers_client::update::Message;
use grammers_client::Client;
use grammers_tl_types as tl;
use log::{debug, info, warn};

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;
use super::extract::extract_community_tag;
use crate::utils::peer_info::{chat_info, sender_info};

/// If the message is a reply and the replied-to message is not yet in ClickHouse,
/// fetch it from Telegram and save it.
pub async fn backfill_reply(client: &Client, message: &Message) {
    let reply_id = match crate::utils::reply_target::reply_target(message) {
        Some(id) => id,
        None => return,
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    if message_exists(chat_id, reply_id).await {
        return;
    }

    if !is_log_ignored(chat_id) {
        debug!("backfill reply_to {} in chat {}", reply_id, chat_id);
    }

    let reply = match client.get_reply_to_message(message).await {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            debug!("reply_to {} not found on Telegram", reply_id);
            return;
        }
        Err(e) => {
            warn!("failed to fetch reply_to {}: {}", reply_id, e);
            return;
        }
    };

    if matches!(reply.raw, tl::enums::Message::Empty(_)) {
        info!("reply_to {} is an empty message, skipping backfill", reply_id);
        return;
    }

    let sender = sender_info(client, &reply).await;
    let chat = chat_info(client, &reply).await;

    let text = crate::utils::format_entities::formatted_text(&reply);
    let sender_bare_id = sender.user_id as i64;
    let msg_content = if !text.is_empty() {
        text
    } else if let Some(action) = reply.action() {
        let sender_display = if sender.second_name.is_empty() {
            sender.first_name.clone()
        } else {
            format!("{} {}", sender.first_name, sender.second_name)
        };
        crate::utils::service_action::format(action, Some(sender_bare_id), Some(&sender_display))
    } else {
        serde_json::to_string(&reply.raw).unwrap_or_default()
    };

    let reply_to = crate::utils::reply_target::reply_target(&reply).unwrap_or(0) as u64;

    crate::db::EVENTS_BUF
        .push(Event {
            date_time: reply.date().as_second() as u32,
            message: msg_content,
            chat_title: chat.chat_title,
            chat_id,
            username: sender.username,
            first_name: sender.first_name,
            second_name: sender.second_name,
            user_id: sender.user_id,
            community_tag: extract_community_tag(&reply.raw),
            message_id: reply.id() as i64,
            chat_usernames: chat.chat_usernames,
            reply_to,
            ..Event::send()
        })
        .await;

    if !is_log_ignored(chat_id) {
        info!(
            "\x1b[96m{:<8} {:>8} backfilled reply_to message\x1b[0m",
            "backfill", reply_id
        );
    }
}

async fn message_exists(chat_id: i64, message_id: i32) -> bool {
    // Check the unflushed buffer
    let in_buf = crate::db::EVENTS_BUF
        .find_last(|e| {
            if e.event == crate::db::SEND && e.chat_id == chat_id && e.message_id == message_id as i64
            {
                Some(())
            } else {
                None
            }
        })
        .await
        .is_some();
    if in_buf {
        return true;
    }

    if let Ok(count) = crate::db::clickhouse()
        .query(
            "SELECT count() FROM events_log \
             WHERE chat_id = ? AND message_id = ? AND event = ?",
        )
        .bind(chat_id)
        .bind(message_id as i64)
        .bind(crate::db::SEND)
        .fetch_one::<u64>()
        .await
    {
        if count > 0 {
            return true;
        }
    }

    false
}
