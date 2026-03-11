use grammers_client::peer::Peer;
use grammers_client::update::Message;
use log::info;

use crate::db::OutgoingMessage;

pub async fn save_outgoing(message: &Message, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let (title, usernames) = match message.peer() {
        Some(Peer::User(user)) => (
            user.username().unwrap_or_default().to_string(),
            user.username().map(|u| vec![u.to_string()]).unwrap_or_default(),
        ),
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
        message.peer_id().bare_id_unchecked().to_string()
    } else {
        title
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    let text = message.text().to_string();
    let raw = serde_json::to_string(&message.raw).unwrap_or_default();
    let reply_to = message.reply_to_message_id().unwrap_or(0) as u64;

    let admins: Vec<String> = Vec::new();

    let media_desc = crate::utils::media_description::describe(message);

    {
        let preview = if !text.is_empty() {
            match &media_desc {
                Some(desc) => format!("{} {}", desc, text),
                None => text.clone(),
            }
        } else {
            media_desc.clone().unwrap_or_default()
        };
        let title_short: String = title.chars().take(25).collect();
        let sender_short: String = crate::db::account_name().chars().take(10).collect::<String>();
        let reply_line = crate::utils::reply_preview::format_reply_line(message).await;
        if !reply_line.is_empty() {
            info!("{}", reply_line);
        }
        info!(
            "\x1b[95m{:<8} {:>6} {:<25} {:<10} {}\x1b[0m",
            "outgoing", message.id(), title_short, sender_short, &preview
        );
    }

    let mut insert = crate::db::clickhouse().insert::<OutgoingMessage>("telegram_messages_new").await?;
    insert.write(&OutgoingMessage {
        date_time: message.date().timestamp() as u32,
        message: text,
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
