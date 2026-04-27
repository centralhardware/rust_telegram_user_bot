use clickhouse::Row;
use grammers_client::peer::Peer;
use grammers_client::update::Message;
use log::info;
use serde::Deserialize;

use crate::db::OutgoingMessage;

#[derive(Row, Deserialize)]
struct LastChatRow {
    title: String,
    usernames: Vec<String>,
}

pub async fn save_outgoing(message: &Message, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let (title, usernames) = match message.peer() {
        Some(Peer::User(user)) => {
            let name = match (user.first_name(), user.last_name()) {
                (Some(first), Some(last)) if !last.is_empty() => format!("{} {}", first, last),
                (Some(first), _) => first.to_string(),
                _ => user.username().unwrap_or_default().to_string(),
            };
            (
                name,
                user.username().map(|u| vec![u.to_string()]).unwrap_or_default(),
            )
        },
        Some(Peer::Group(group)) => (
            group.title().unwrap_or_default().to_string(),
            group.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        Some(Peer::Channel(channel)) => (
            channel.title().to_string(),
            channel.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        None => (String::new(), Vec::new()),
    };

    let title = if title.is_empty() {
        message.peer()
            .and_then(|p| p.name().map(|s| s.to_string()))
            .unwrap_or_default()
    } else {
        title
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    let (title, usernames) = if title.is_empty() {
        match crate::db::clickhouse()
            .query("SELECT title, usernames FROM telegram_messages_new WHERE id = ? AND title != '' ORDER BY date_time DESC LIMIT 1")
            .bind(chat_id)
            .fetch_one::<LastChatRow>()
            .await
        {
            Ok(row) => (row.title, row.usernames),
            Err(_) => (title, usernames),
        }
    } else {
        (title, usernames)
    };

    let text = crate::utils::format_entities::formatted_text(message);
    let raw = serde_json::to_string(&message.raw).unwrap_or_default();
    let reply_to = message.reply_to_message_id().unwrap_or(0) as u64;

    let admins: Vec<String> = Vec::new();

    let media_desc = crate::utils::media_description::describe(message);
    let buttons = crate::utils::inline_buttons::format_buttons(message);
    let sender_id = message.sender_id().map(|p| p.bare_id_unchecked());
    let sender_name = message.sender().map(|p| match p {
        Peer::User(u) => u.full_name(),
        _ => p.name().unwrap_or_default().to_string(),
    });
    let action_desc = if text.is_empty() {
        message.action().map(|a| crate::utils::service_action::format(a, sender_id, sender_name.as_deref()))
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
        let title_short: String = title.chars().take(25).collect();
        let reply_line = crate::utils::reply_preview::format_reply_line(message).await;
        if !reply_line.is_empty() {
            info!("{}", reply_line);
        }
        info!(
            "\x1b[95m{:<8} {:>8} {:<25} \x1b[90m│\x1b[95m {:<10} \x1b[90m│\x1b[95m {}\x1b[0m",
            "outgoing", message.id(), title_short, "", &preview
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

    let mut insert = crate::db::clickhouse().insert::<OutgoingMessage>("telegram_messages_new").await?;
    insert.write(&OutgoingMessage {
        date_time: message.date().timestamp() as u32,
        message: msg_content,
        title,
        id: chat_id,
        admins2: admins,
        usernames,
        message_id: message.id() as u64,
        reply_to,
        raw,
        client_id,
    }).await?;
    insert.end().await?;

    Ok(())
}
