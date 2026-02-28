use grammers_client::peer::Peer;
use grammers_client::update::Message;
use grammers_client::Client;
use log::info;

use crate::db::OutgoingMessage;
use crate::handlers::{get_topic_id, get_topic_name};

pub async fn save_outgoing(message: &Message, client: &Client, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    if !message.outgoing() {
        return Ok(());
    }

    let peer = match message.peer() {
        Some(p) => p,
        None => return Ok(()),
    };

    let (title, usernames) = match &peer {
        Peer::User(user) => (
            user.username().unwrap_or_default().to_string(),
            user.username().map(|u| vec![u.to_string()]).unwrap_or_default(),
        ),
        Peer::Group(group) => (
            group.title().unwrap_or_default().to_string(),
            group.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        Peer::Channel(channel) => (
            channel.title().to_string(),
            channel.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
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

    let topic_id = get_topic_id(message);
    let topic_name = match topic_id {
        Some(id) => get_topic_name(client, message, id).await,
        None => String::new(),
    };

    {
        let preview: String = text.chars().take(80).collect();
        let title_short: String = title.chars().take(25).collect();
        let reply_part = message.reply_to_message_id()
            .filter(|id| topic_id != Some(*id))
            .map(|id| format!(" reply to {id}"))
            .unwrap_or_default();
        let topic_part = if topic_name.is_empty() {
            String::new()
        } else {
            format!(" [{topic_name}]")
        };
        info!(
            "\x1b[95m{:<15} {:>5} {:<25}{} {}{}\x1b[0m",
            "outgoing", message.id(), title_short, topic_part, preview, reply_part
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
        topic_id: topic_id.unwrap_or(0),
        topic_name,
    }).await?;
    insert.end().await?;

    Ok(())
}
