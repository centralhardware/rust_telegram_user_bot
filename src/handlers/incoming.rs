use clickhouse::Row;
use grammers_client::peer::Peer;
use grammers_client::update::Message;
use serde::Serialize;

use crate::schedulers;

#[derive(Row, Serialize)]
struct IncomingMessage {
    date_time: u32,
    message: String,
    chat_title: String,
    chat_id: i64,
    username: Vec<String>,
    first_name: String,
    second_name: String,
    user_id: u64,
    message_id: i64,
    chat_usernames: Vec<String>,
    reply_to: u64,
    client_id: u64,
}

pub async fn save_incoming(message: &Message, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    if message.outgoing() {
        return Ok(());
    }

    {
        let chat_name = message.peer()
            .map(|p| p.name().unwrap_or_default().to_string())
            .unwrap_or_default();
        let text = message.text();
        let preview = text;
        let reply_part = message.reply_to_message_id()
            .map(|id| format!(" reply to {id}"))
            .unwrap_or_default();
        println!(
            "\x1b[92m{:<15} {:>5} {:<25} {}{}\x1b[0m",
            "incoming", message.id(), chat_name, preview, reply_part
        );
    }

    let peer = match message.peer() {
        Some(p) => p,
        None => return Ok(()),
    };

    // Skip private chats (only save group/channel messages)
    if matches!(peer, Peer::User(_)) {
        return Ok(());
    }

    let sender = match message.sender() {
        Some(s) => s,
        None => return Ok(()),
    };

    let (username, first_name, second_name, user_id) = match &sender {
        Peer::User(user) => (
            vec![user.username().unwrap_or_default().to_string()],
            user.first_name().unwrap_or_default().to_string(),
            user.last_name().unwrap_or_default().to_string(),
            user.id().bare_id_unchecked() as u64,
        ),
        _ => (Vec::new(), String::new(), String::new(), 0),
    };

    let (chat_title, chat_usernames) = match peer {
        Peer::Group(group) => (
            group.title().unwrap_or_default().to_string(),
            group.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        Peer::Channel(channel) => (
            channel.title().to_string(),
            channel.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        _ => (String::new(), Vec::new()),
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    let text = message.text();
    let msg_content = if text.is_empty() {
        serde_json::to_string(&message.raw).unwrap_or_default()
    } else {
        text.to_string()
    };

    let reply_to = message.reply_to_message_id().unwrap_or(0) as u64;

    let clickhouse_client = schedulers::clickhouse_client()?;
    let mut insert = clickhouse_client.insert::<IncomingMessage>("chats_log").await?;
    insert.write(&IncomingMessage {
        date_time: message.date().timestamp() as u32,
        message: msg_content,
        chat_title,
        chat_id,
        username,
        first_name,
        second_name,
        user_id,
        message_id: message.id() as i64,
        chat_usernames,
        reply_to,
        client_id,
    }).await?;
    insert.end().await?;

    Ok(())
}
