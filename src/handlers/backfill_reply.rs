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
    let quoted = crate::utils::reply_target::reply_info(message);
    let reply_id = match quoted.reply_to {
        0 => return,
        id => id as i32,
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    // A quote of another chat names a message id over there. Backfilling it here
    // would look for it in the wrong chat and, if that id happens to exist,
    // write the wrong message under it — so leave the quote to `quote_text`.
    if quoted.reply_to_chat_id != 0 {
        debug!(
            "reply_to {} is quoted from chat {}, not backfilling",
            reply_id, quoted.reply_to_chat_id
        );
        return;
    }

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
        crate::utils::service_action::describe(&reply, action, Some(sender_bare_id), Some(&sender_display)).await
    } else {
        serde_json::to_string(&reply.raw).unwrap_or_default()
    };

    let mut reply_reply = crate::utils::reply_target::reply_info(&reply);
    let reply_to_user_id = crate::db::resolve_reply(chat_id, &mut reply_reply).await;
    let (topic_id, topic_name) = crate::utils::topic::topic_of(client, &reply).await;

    // A backfilled message is a message: it gets the same columns a live one
    // gets, or the row would quietly be the thinner of the two.
    let meta = crate::utils::media_description::media_meta_of(&reply.raw).unwrap_or_default();
    let meta_msg = crate::utils::message_meta::of(&reply.raw);

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
            community_id: chat.community_id,
            message_id: reply.id() as i64,
            chat_usernames: chat.chat_usernames,
            // A backfilled message can be one this account sent: `Event::send()`
            // defaults to incoming, which would be wrong for half of them.
            out: crate::utils::self_id::is_outgoing(&reply),
            reply_to: reply_reply.reply_to,
            reply_to_user_id,
            reply_to_chat_id: reply_reply.reply_to_chat_id,
            quote_text: reply_reply.quote_text,
        comment_to: reply_reply.comment_to,
            topic_id,
            topic_name,
            raw: serde_json::to_string(&reply.raw).unwrap_or_default(),
            media_type: meta.media_type,
            file_name: meta.file_name,
            mime_type: meta.mime_type,
            size: meta.size,
            duration: meta.duration,
            width: meta.width,
            height: meta.height,
            lat: meta.lat,
            lon: meta.lon,
            poll_question: meta.poll_question,
            poll_options: meta.poll_options,
            fwd_from_user_id: meta_msg.fwd_from_user_id,
            fwd_from_chat_id: meta_msg.fwd_from_chat_id,
            fwd_from_msg_id: meta_msg.fwd_from_msg_id,
            fwd_from_name: meta_msg.fwd_from_name,
            fwd_date: meta_msg.fwd_date,
            action: meta_msg.action,
            grouped_id: meta_msg.grouped_id,
            via_bot_id: meta_msg.via_bot_id,
            guest_from_id: meta_msg.guest_from_id,
            post_author: meta_msg.post_author,
            pinned: meta_msg.pinned,
            silent: meta_msg.silent,
            noforwards: meta_msg.noforwards,
            ttl_period: meta_msg.ttl_period,
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
            // An ephemeral id names a different message entirely, so one must
            // never answer for an ordinary id.
            if e.event == crate::db::SEND
                && !e.ephemeral
                && e.chat_id == chat_id
                && e.message_id == message_id as i64
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
             WHERE chat_id = ? AND message_id = ? AND event = ? AND NOT ephemeral",
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
