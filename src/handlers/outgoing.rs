use clickhouse::Row;
use grammers_client::peer::Peer;
use grammers_client::update::Message;
use grammers_client::Client;
use serde::Deserialize;

use crate::db::Event;
use super::extract::ChatInfo;
use super::send::Body;

#[derive(Row, Deserialize)]
struct LastChatRow {
    title: String,
    usernames: Vec<String>,
}

pub async fn save_outgoing(message: &Message, client: &Client, me: u64) -> Result<Event, Box<dyn std::error::Error>> {
    let chat = crate::utils::peer_info::chat_info(message).await;
    let community_id = chat.community_id;
    let (title, usernames) = (chat.chat_title, chat.chat_usernames);

    let chat_id = message.peer_id().bare_id_unchecked();

    // A chat Telegram would not name for us, and `peer_names` has no name for,
    // is still recognizable by whatever name it last went by here. Read from
    // the buffer like every other read, so a title logged a moment ago counts,
    // and with argMax rather than a sort of the chat's whole history.
    let (title, usernames) = if title.is_empty() {
        match crate::db::clickhouse()
            .query(
                "SELECT argMax(chat_title, date_time) AS title, \
                        argMax(chat_usernames, date_time) AS usernames \
                 FROM events_log_buffer \
                 WHERE chat_id = ? AND event = ? AND chat_title != ''",
            )
            .bind(chat_id)
            .bind(crate::db::EventKind::Send)
            .fetch_one::<LastChatRow>()
            .await
        {
            // With no row to aggregate the title comes back empty: no name.
            Ok(row) if !row.title.is_empty() => (row.title, row.usernames),
            _ => (title, usernames),
        }
    } else {
        (title, usernames)
    };

    let sender_id = message.sender_id().map(|p| p.bare_id_unchecked());
    let sender_name = message.sender().map(|p| match p {
        Peer::User(u) => u.full_name(),
        _ => p.name().unwrap_or_default().to_string(),
    });
    let body = Body::of(client, message, sender_id, sender_name.as_deref()).await;

    super::send::print(client, message, &body, ("outgoing", "95"), &title, "").await;

    let chat = ChatInfo {
        chat_title: title,
        chat_usernames: usernames,
        community_id,
    };
    let event = Event {
        // The account's own message.
        user_id: me,
        out: true,
        ..super::send::event(client, message, &body, chat).await
    };

    crate::db::log_event(event.clone()).await;

    Ok(event)
}
