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

    let original = crate::db::find_message(chat_id, msg_id).await.message;

    if original.is_empty() || original == message_content {
        return Ok(());
    }

    let diff = crate::utils::diff::word_patch(&original, &message_content);

    let sender = crate::utils::peer_info::sender_info(client, message).await;

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

    // Telegram's own edit time, not the moment this process got round to it: the
    // row is when the message changed, and a reconnect replaying a backlog of
    // edits must not stamp them all with the time it caught up.
    let now = match message.edit_date() {
        Some(date) => date.as_second() as u32,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as u32,
    };

    // An edit row carries only what an edit can change: the message as it now
    // stands, the patch against what stood before -- the words that went and the
    // words that came, and nothing that stayed, which `edit_diff_html` turns back
    // into the marked-up message a board prints -- and the media the text
    // describes. Everything else is fixed when the message is sent and already on
    // its send row.
    let meta = crate::utils::media_description::media_meta(message).unwrap_or_default();

    crate::db::EVENTS_BUF.push(Event {
        date_time: now,
        chat_id,
        message_id: msg_id,
        message: message_content,
        diff,
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
        ..Event::edit()
    }).await;

    Ok(())
}
