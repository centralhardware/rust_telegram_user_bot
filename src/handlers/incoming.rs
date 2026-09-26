use grammers_client::update::Message;

use crate::db::Event;
use super::extract::extract_community_tag_from_update;
use super::send::Body;
use crate::utils::peer_info::{chat_info, sender_info};
use crate::app::App;

pub async fn save_incoming(app: &App, message: &Message) -> Result<Event, Box<dyn std::error::Error>> {
    let sender = sender_info(app, message).await;
    let chat = chat_info(app, message).await;
    let chat_id = message.peer_id().bare_id_unchecked();

    let sender_display = if sender.second_name.is_empty() {
        sender.first_name.clone()
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    };
    let body = Body::of(app, message, Some(sender.user_id as i64), Some(&sender_display)).await;

    if !app.is_log_ignored(chat_id) {
        super::send::print(app, message, &body, ("incoming", crate::utils::console::Tone::Incoming), &chat.chat_title, &sender_display).await;
    }

    let event = Event {
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        user_id: sender.user_id,
        community_tag: extract_community_tag_from_update(&message.raw),
        ..super::send::event(app, message, &body, chat).await
    };

    app.db.log_event(event.clone()).await;

    Ok(event)
}
