use grammers_client::update::Message;
use grammers_client::Client;
use grammers_tl_types as tl;
use log::{debug, info, warn};

use crate::db::IncomingMessage;
use crate::utils::log_ignore::is_log_ignored;
use super::extract::{extract_sender, extract_chat, extract_community_tag};

/// If the message is a reply and the replied-to message is not yet in ClickHouse,
/// fetch it from Telegram and save it.
pub async fn backfill_reply(client: &Client, message: &Message, client_id: u64) {
    let reply_id = match message.reply_to_message_id() {
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

    let sender = extract_sender(&reply);
    let chat = extract_chat(&reply);

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

    let reply_to = reply.reply_to_message_id().unwrap_or(0) as u64;

    crate::db::INCOMING_BUF
        .push(IncomingMessage {
            date_time: reply.date().timestamp() as u32,
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
            client_id,
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
    // Check unflushed incoming buffer
    let in_buf = crate::db::INCOMING_BUF
        .find_last(|m| {
            if m.chat_id == chat_id && m.message_id == message_id as i64 {
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

    let db = crate::db::clickhouse();

    if let Ok(count) = db
        .query(
            "SELECT sum(c) AS cnt FROM (\
                SELECT count() AS c FROM chats_log WHERE chat_id = ? AND message_id = ? \
                UNION ALL \
                SELECT count() AS c FROM telegram_messages_new WHERE id = ? AND message_id = ?\
            )",
        )
        .bind(chat_id)
        .bind(message_id as i64)
        .bind(chat_id)
        .bind(message_id as u64)
        .fetch_one::<u64>()
        .await
    {
        if count > 0 {
            return true;
        }
    }

    false
}
