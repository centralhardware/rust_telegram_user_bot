use grammers_client::update::Message;
use log::info;

use crate::db::IncomingMessage;
use super::extract::{extract_sender, extract_chat, extract_community_tag_from_update};

pub async fn save_incoming(message: &Message, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let media_desc = crate::utils::media_description::describe(message);

    let sender = extract_sender(message);
    let chat = extract_chat(message);
    let community_tag = extract_community_tag_from_update(&message.raw);

    let chat_id = message.peer_id().bare_id_unchecked();

    let sender_display = if sender.second_name.is_empty() {
        sender.first_name.clone()
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    };

    {
        let text = crate::utils::format_entities::formatted_text(message);
        let sender_bare_id = sender.user_id as i64;
        let action_desc = if text.is_empty() {
            message.action().map(|a| crate::utils::service_action::format(a, Some(sender_bare_id), Some(&sender_display)))
        } else {
            None
        };
        let preview = if !text.is_empty() {
            match &media_desc {
                Some(desc) => format!("{} {}", desc, text),
                None => text.to_string(),
            }
        } else if let Some(ref desc) = action_desc {
            desc.clone()
        } else {
            media_desc.clone().unwrap_or_default()
        };
        let sender_short: String = sender_display.chars().take(10).collect();
        let chat_name_short: String = chat.chat_title.chars().take(25).collect();

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
    let msg_content = if text.is_empty() {
        if let Some(action) = message.action() {
            crate::utils::service_action::format(action, Some(sender_bare_id), Some(&sender_display))
        } else {
            serde_json::to_string(&message.raw).unwrap_or_default()
        }
    } else {
        text.to_string()
    };

    let reply_to = message.reply_to_message_id().unwrap_or(0) as u64;

    crate::db::INCOMING_BUF.push(IncomingMessage {
        date_time: message.date().timestamp() as u32,
        message: msg_content,
        chat_title: chat.chat_title,
        chat_id,
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        user_id: sender.user_id,
        community_tag,
        message_id: message.id() as i64,
        chat_usernames: chat.chat_usernames,
        reply_to,
        client_id,
    }).await;

    Ok(())
}
