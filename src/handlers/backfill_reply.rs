use grammers_client::peer::Peer;
use grammers_client::update::Message;
use grammers_client::Client;
use log::{debug, info, warn};

use crate::db::IncomingMessage;

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

    debug!("backfill reply_to {} in chat {}", reply_id, chat_id);

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

    let (username, first_name, second_name, user_id) = match reply.sender() {
        Some(Peer::User(user)) => (
            vec![user.username().unwrap_or_default().to_string()],
            user.first_name().unwrap_or_default().to_string(),
            user.last_name().unwrap_or_default().to_string(),
            user.id().bare_id_unchecked() as u64,
        ),
        _ => (Vec::new(), String::new(), String::new(), 0),
    };

    let (chat_title, chat_usernames) = match reply.peer() {
        Some(Peer::Group(group)) => (
            group.title().unwrap_or_default().to_string(),
            group.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        Some(Peer::Channel(channel)) => (
            channel.title().to_string(),
            channel.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        _ => (String::new(), Vec::new()),
    };

    let chat_title = if chat_title.is_empty() {
        reply
            .peer()
            .map(|p| p.name().unwrap_or_default().to_string())
            .unwrap_or_default()
    } else {
        chat_title
    };

    let text = crate::utils::format_entities::formatted_text(&reply);
    let sender_bare_id = user_id as i64;
    let msg_content = if !text.is_empty() {
        text
    } else if let Some(action) = reply.action() {
        let sender_display = if second_name.is_empty() {
            first_name.clone()
        } else {
            format!("{} {}", first_name, second_name)
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
            chat_title,
            chat_id,
            username,
            first_name,
            second_name,
            user_id,
            message_id: reply.id() as i64,
            chat_usernames,
            reply_to,
            client_id,
        })
        .await;

    info!(
        "\x1b[96m{:<8} {:>8} backfilled reply_to message\x1b[0m",
        "backfill", reply_id
    );
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
        .query("SELECT count() FROM chats_log WHERE chat_id = ? AND message_id = ?")
        .bind(chat_id)
        .bind(message_id as i64)
        .fetch_one::<u64>()
        .await
    {
        if count > 0 {
            return true;
        }
    }

    if let Ok(count) = db
        .query("SELECT count() FROM telegram_messages_new WHERE id = ? AND message_id = ?")
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
