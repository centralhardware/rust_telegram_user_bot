use grammers_client::peer::Peer;
use grammers_client::update::Message;
use log::info;

use crate::db::IncomingMessage;

pub async fn save_incoming(message: &Message, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let media_desc = crate::utils::media_description::describe(message);

    let (username, first_name, second_name, user_id) = match message.sender() {
        Some(Peer::User(user)) => (
            vec![user.username().unwrap_or_default().to_string()],
            user.first_name().unwrap_or_default().to_string(),
            user.last_name().unwrap_or_default().to_string(),
            user.id().bare_id_unchecked() as u64,
        ),
        _ => (Vec::new(), String::new(), String::new(), 0),
    };

    let (chat_title, chat_usernames) = match message.peer() {
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

    let chat_name = if chat_title.is_empty() {
        message.peer()
            .map(|p| p.name().unwrap_or_default().to_string())
            .unwrap_or_default()
    } else {
        chat_title.clone()
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    {
        let text = message.text();
        let action_desc = if text.is_empty() {
            message.action().map(|a| crate::utils::service_action::format(a))
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

        let sender_display = if second_name.is_empty() {
            first_name.clone()
        } else {
            format!("{} {}", first_name, second_name)
        };
        let sender_short: String = sender_display.chars().take(10).collect();
        let chat_name_short: String = chat_name.chars().take(25).collect();

        let reply_line = crate::utils::reply_preview::format_reply_line(message).await;
        if !reply_line.is_empty() {
            info!("{}", reply_line);
        }
        info!(
            "\x1b[92m{:<8} {:>6} {:<25} {:<10} {}\x1b[0m",
            "incoming", message.id(), chat_name_short, sender_short, &preview
        );
    }

    let text = message.text();
    let msg_content = if text.is_empty() {
        if let Some(action) = message.action() {
            crate::utils::service_action::format(action)
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
        chat_title: if chat_title.is_empty() { chat_name } else { chat_title },
        chat_id,
        username,
        first_name,
        second_name,
        user_id,
        message_id: message.id() as i64,
        chat_usernames,
        reply_to,
        client_id,
    }).await;

    Ok(())
}
