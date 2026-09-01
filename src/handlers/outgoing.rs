use clickhouse::Row;
use grammers_client::Client;
use grammers_client::peer::Peer;
use grammers_client::update::Message;
use log::info;
use serde::Deserialize;

use crate::db::Event;

#[derive(Row, Deserialize)]
struct LastChatRow {
    chat_title: String,
    chat_usernames: Vec<String>,
}

pub async fn save_outgoing(
    message: &Message,
    client: &Client,
    me: u64,
) -> Result<Event, Box<dyn std::error::Error>> {
    let chat = crate::utils::peer_info::chat_info(client, message).await;
    let community_id = chat.community_id;
    let (title, usernames) = (chat.chat_title, chat.chat_usernames);

    let chat_id = message.peer_id().bare_id_unchecked();

    // A chat Telegram would not name for us is still recognizable by whatever
    // name it last went by here.
    let (title, usernames) = if title.is_empty() {
        match crate::db::clickhouse()
            .query(
                "SELECT chat_title, chat_usernames FROM events_log \
                 WHERE chat_id = ? AND event = ? AND chat_title != '' \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(crate::db::SEND)
            .fetch_one::<LastChatRow>()
            .await
        {
            Ok(row) => (row.chat_title, row.chat_usernames),
            Err(_) => (title, usernames),
        }
    } else {
        (title, usernames)
    };

    let text = crate::utils::format_entities::formatted_text(message);
    let raw = serde_json::to_string(&message.raw).unwrap_or_default();
    let reply = crate::utils::reply_target::reply_info(message);

    let media_desc = crate::utils::media_description::describe(message);
    let buttons = crate::utils::inline_buttons::format_buttons(message);
    let sender_id = message.sender_id().map(|p| p.bare_id_unchecked());
    let sender_name = message.sender().map(|p| match p {
        Peer::User(u) => u.full_name(),
        _ => p.name().unwrap_or_default().to_string(),
    });
    let action_desc = if text.is_empty() {
        message
            .action()
            .map(|a| crate::utils::service_action::format(a, sender_id, sender_name.as_deref()))
    } else {
        None
    };

    let mut preview = if !text.is_empty() {
        match &media_desc {
            Some(desc) => format!("{} {}", desc, text),
            None => text.clone(),
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

    {
        let topic_name = crate::utils::topic::topic_name(client, message).await;
        let title_short: String = if topic_name.is_empty() {
            title.chars().take(25).collect()
        } else {
            format!("{} / {}", title, topic_name)
                .chars()
                .take(25)
                .collect()
        };
        let reply_line = crate::utils::reply_preview::format_reply_line(message).await;
        if !reply_line.is_empty() {
            info!("{}", reply_line);
        }
        info!(
            "\x1b[95m{:<8} {:>8} {:<25} \x1b[90m│\x1b[95m {:<10} \x1b[90m│\x1b[95m {}\x1b[0m",
            "outgoing",
            message.id(),
            title_short,
            "",
            &preview
        );
    }

    let mut msg_content = if !text.is_empty() {
        text
    } else if let Some(ref desc) = action_desc {
        desc.clone()
    } else {
        media_desc.unwrap_or_default()
    };
    if let Some(b) = &buttons {
        if !msg_content.is_empty() {
            msg_content.push_str("\n\n");
        }
        msg_content.push_str(b);
    }

    let reply_to_user_id = crate::db::find_reply_sender(chat_id, &reply).await;
    let (topic_id, topic_name) = crate::utils::topic::topic_of(client, message).await;

    let meta = crate::utils::media_description::media_meta(message).unwrap_or_default();
    let meta_msg = crate::utils::message_meta::of(&std::ops::Deref::deref(message).raw);

    let event = Event {
        date_time: message.date().as_second() as u32,
        message: msg_content,
        chat_title: title,
        chat_id,
        chat_usernames: usernames,
        community_id,
        message_id: message.id() as i64,
        // The account's own message.
        user_id: me,
        out: true,
        reply_to: reply.reply_to,
        reply_to_user_id,
        reply_to_chat_id: reply.reply_to_chat_id,
        quote_text: reply.quote_text,
        topic_id,
        topic_name,
        raw,
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
