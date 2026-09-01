use grammers_client::update::Message;
use grammers_client::Client;
use log::info;

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;

pub async fn save_edited(
    message: &Message,
    client: &Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let chat_id = message.peer_id().bare_id_unchecked();
    let msg_id = message.id() as i64;
    let mut message_content = crate::utils::format_entities::formatted_text(message);
    if let Some(b) = crate::utils::inline_buttons::format_buttons(message) {
        if !message_content.is_empty() {
            message_content.push_str("\n\n");
        }
        message_content.push_str(&b);
    }

    if message_content.is_empty() {
        return Ok(());
    }

    let original = crate::db::find_message(chat_id, msg_id).await;

    if original.is_empty() || original == message_content {
        return Ok(());
    }

    let diff = crate::utils::diff::html_diff(&original, &message_content);

    let sender = crate::utils::peer_info::sender_info(client, message).await;
    let user_id = sender.user_id;

    let reply = crate::utils::reply_target::reply_info(message);
    let reply_to_user_id = crate::db::find_reply_sender(chat_id, &reply).await;

    let chat = crate::utils::peer_info::chat_info(client, message).await;
    let chat_name = chat.chat_title.clone();
    let sender_name = if sender.second_name.is_empty() {
        sender.first_name.clone()
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    };
    let sender_short: String = sender_name.chars().take(10).collect();

    if !is_log_ignored(chat_id) {
        let chat_name_short: String = chat_name.chars().take(25).collect();
        let colored = crate::utils::diff::inline_diff(&original, &message_content);
        info!(
            "\x1b[93m{:<8} {:>8} {:<25} \x1b[90m│\x1b[93m {:<10}\x1b[0m\n{}",
            "edited",
            message.id(),
            chat_name_short,
            sender_short,
            colored,
        );
    }

    let (topic_id, topic_name) = crate::utils::topic::topic_of(client, message).await;

    // An edit is the message as it now stands, so the row carries what a send row
    // carries: the object itself, whatever media it holds, and the rest of what
    // Telegram says about it. Only `diff` is the edit's own -- the same inline
    // rendering the console line above shows, in HTML, so a board only has to
    // print it.
    let meta = crate::utils::media_description::media_meta(message).unwrap_or_default();
    let meta_msg = crate::utils::message_meta::of(&std::ops::Deref::deref(message).raw);

    // Telegram's own edit time, not the moment this process got round to it: the
    // row is when the message changed, and a reconnect replaying a backlog of
    // edits must not stamp them all with the time it caught up.
    let now = match message.edit_date() {
        Some(date) => date.as_second() as u32,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as u32,
    };

    crate::db::EVENTS_BUF.push(Event {
        date_time: now,
        chat_id,
        chat_title: chat_name,
        message_id: msg_id,
        message: message_content,
        diff,
        raw: serde_json::to_string(&message.raw).unwrap_or_default(),
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        community_tag: super::extract::extract_community_tag_from_update(&message.raw),
        reply_to: reply.reply_to,
        reply_to_user_id,
        reply_to_chat_id: reply.reply_to_chat_id,
        quote_text: reply.quote_text,
        chat_usernames: chat.chat_usernames,
        community_id: chat.community_id,
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
        // An edit of the account's own message, so `out` means the same thing on
        // an edit row as it does on the send it follows.
        out: crate::utils::self_id::is_outgoing(message),
        topic_id,
        topic_name,
        user_id,
        ..Event::edit()
    }).await;

    Ok(())
}
