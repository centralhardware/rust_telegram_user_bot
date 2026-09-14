//! Turn a message fetched from Telegram into the `events_log` row it would have
//! got had it arrived as an update.
//!
//! A message the account was not listening for — the target of a reply, or one
//! pulled out of a chat's history by a backfill — has to be logged with the same
//! columns a live one gets, or the row is quietly the thinner of the two. Both
//! callers build it here so there is one answer to what a row holds.

use grammers_client::Client;
use grammers_client::message::Message;

use crate::db::Event;
use crate::handlers::extract::extract_community_tag;
use crate::utils::peer_info::{chat_info, sender_info};

pub async fn event_of(client: &Client, msg: &Message) -> Event {
    let chat_id = msg.peer_id().bare_id_unchecked();

    let sender = sender_info(msg).await;
    let chat = chat_info(msg).await;

    let text = crate::utils::format_entities::plain_text(msg);
    let sender_bare_id = sender.user_id as i64;
    let message = if !text.is_empty() {
        text
    } else if let Some(action) = msg.action() {
        let sender_display = if sender.second_name.is_empty() {
            sender.first_name.clone()
        } else {
            format!("{} {}", sender.first_name, sender.second_name)
        };
        let game_title = crate::utils::service_action::game_title(client, msg).await;
        crate::utils::service_action::format(
            action,
            Some(sender_bare_id),
            Some(&sender_display),
            game_title.as_deref(),
        )
    } else if let Some(media) = crate::utils::media_description::describe_of(&msg.raw) {
        // What the live path writes for a message that is a photo, a voice note,
        // a sticker: the description, not the message's wire form. The raw JSON
        // below is a last resort for a message that is none of the three, and
        // was standing in for this one.
        media
    } else {
        serde_json::to_string(&msg.raw).unwrap_or_default()
    };

    let mut reply = crate::utils::reply_target::reply_info(msg);
    let reply_to_user_id = crate::db::resolve_reply(chat_id, &mut reply).await;
    let (topic_id, topic_name) = crate::utils::topic::topic_of(client, msg).await;

    let meta = crate::utils::media_description::media_meta_of(&msg.raw).unwrap_or_default();
    let meta_msg = crate::utils::message_meta::of(&msg.raw);

    Event {
        date_time: msg.date().as_second() as u32,
        message,
        entities: crate::utils::entities::of_message(msg),
        keyboard: crate::utils::entities::keyboard_of_raw(&msg.raw),
        chat_title: chat.chat_title,
        chat_id,
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        user_id: sender.user_id,
        community_tag: extract_community_tag(&msg.raw),
        community_id: chat.community_id,
        message_id: msg.id() as i64,
        chat_usernames: chat.chat_usernames,
        // A fetched message can be one this account sent: `Event::send()` defaults
        // to incoming, which would be wrong for half of them.
        out: crate::utils::self_id::is_outgoing(msg),
        reply_to: reply.reply_to,
        reply_to_user_id,
        reply_to_chat_id: reply.reply_to_chat_id,
        quote_text: reply.quote_text,
        comment_to: reply.comment_to,
        topic_id,
        topic_name,
        raw: serde_json::to_string(&msg.raw).unwrap_or_default(),
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
        poll_id: meta.poll_id,
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
    }
}
