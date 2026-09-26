use grammers_client::update::Message;
use grammers_client::Client;

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;
use super::extract::extract_community_tag_from_update;
use super::send::Body;
use crate::utils::peer_info::{chat_info, sender_info};

pub async fn save_incoming(message: &Message, client: &Client) -> Result<Event, Box<dyn std::error::Error>> {
    let sender = sender_info(message).await;
    let chat = chat_info(message).await;
    let chat_id = message.peer_id().bare_id_unchecked();

    let sender_display = if sender.second_name.is_empty() {
        sender.first_name.clone()
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    };
    let body = Body::of(client, message, Some(sender.user_id as i64), Some(&sender_display)).await;

    if !is_log_ignored(chat_id) {
        super::send::print(client, message, &body, ("incoming", "92"), &chat.chat_title, &sender_display).await;
    }

    let event = Event {
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        user_id: sender.user_id,
        community_tag: extract_community_tag_from_update(&message.raw),
        ..super::send::event(client, message, &body, chat).await
    };

    crate::db::log_event(event.clone()).await;

    Ok(event)
}
