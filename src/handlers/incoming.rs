use grammers_client::update::Message;
use grammers_client::Client;
use log::info;

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;
use super::extract::extract_community_tag_from_update;
use crate::utils::peer_info::{chat_info, sender_info};

pub async fn save_incoming(message: &Message, client: &Client) -> Result<Event, Box<dyn std::error::Error>> {
    let media_desc = crate::utils::media_description::describe(message);

    let sender = sender_info(client, message).await;
    let chat = chat_info(client, message).await;
    let community_tag = extract_community_tag_from_update(&message.raw);
    let buttons = crate::utils::inline_buttons::format_buttons(message);

    let chat_id = message.peer_id().bare_id_unchecked();

    let sender_display = if sender.second_name.is_empty() {
        sender.first_name.clone()
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    };

    if !is_log_ignored(chat_id) {
        let text = crate::utils::format_entities::formatted_text(message);
        let sender_bare_id = sender.user_id as i64;
        let action_desc = if text.is_empty() {
            message.action().map(|a| crate::utils::service_action::format(a, Some(sender_bare_id), Some(&sender_display)))
        } else {
            None
        };
        let mut preview = if !text.is_empty() {
            match &media_desc {
                Some(desc) => format!("{} {}", desc, text),
                None => text.to_string(),
            }
        } else if let Some(ref desc) = action_desc {
            desc.clone()
        } else {
            media_desc.clone().unwrap_or_default()
        };
        if let Some(b) = &buttons {
            if !preview.is_empty() {
                preview.push_str("\n\n");
            }
            preview.push_str(b);
        }
        let sender_short: String = sender_display.chars().take(10).collect();
        let topic_name = crate::utils::topic::topic_name(client, message).await;
        let chat_name_short: String = if topic_name.is_empty() {
            chat.chat_title.chars().take(25).collect()
        } else {
            format!("{} / {}", chat.chat_title, topic_name).chars().take(25).collect()
        };

        let reply_line = crate::utils::reply_preview::format_reply_line(message).await;
        if !reply_line.is_empty() {
            info!("{}", reply_line);
        }
        info!(
            "\x1b[92m{:<8} {:>8} {:<25} \x1b[90m│\x1b[92m {:<10} \x1b[90m│\x1b[92m {}\x1b[0m",
            "incoming", message.id(), chat_name_short, sender_short, &preview
        );
    }

    let text = crate::utils::format_entities::formatted_text(message);
    let sender_bare_id = sender.user_id as i64;
    let mut msg_content = if text.is_empty() {
        if let Some(action) = message.action() {
            crate::utils::service_action::format(action, Some(sender_bare_id), Some(&sender_display))
        } else {
            media_desc.clone().unwrap_or_default()
        }
    } else {
        text.to_string()
    };
    if let Some(b) = &buttons {
        if !msg_content.is_empty() {
            msg_content.push_str("\n\n");
        }
        msg_content.push_str(b);
    }

    let mut reply = crate::utils::reply_target::reply_info(message);
    let reply_to_user_id = crate::db::resolve_reply(chat_id, &mut reply).await;
    let (topic_id, topic_name) = crate::utils::topic::topic_of(client, message).await;

    let meta = crate::utils::media_description::media_meta(message).unwrap_or_default();
    let meta_msg = crate::utils::message_meta::of(&std::ops::Deref::deref(message).raw);

    let event = Event {
        date_time: message.date().as_second() as u32,
        message: msg_content,
        chat_title: chat.chat_title,
        chat_id,
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        user_id: sender.user_id,
        community_tag,
        community_id: chat.community_id,
        message_id: message.id() as i64,
        chat_usernames: chat.chat_usernames,
        reply_to: reply.reply_to,
        reply_to_user_id,
        reply_to_chat_id: reply.reply_to_chat_id,
        quote_text: reply.quote_text,
        comment_to: reply.comment_to,
        topic_id,
        topic_name,
        raw: serde_json::to_string(&message.raw).unwrap_or_default(),
        media_type: meta.media_type,
        file_name: meta.file_name,
        mime_type: meta.mime_type,
        size: meta.size,
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
        duration: meta.duration,
        width: meta.width,
        height: meta.height,
        lat: meta.lat,
        lon: meta.lon,
        poll_question: meta.poll_question,
        poll_options: meta.poll_options,
        ..Event::send()
    };

    crate::db::EVENTS_BUF.push(event.clone()).await;

    Ok(event)
}
