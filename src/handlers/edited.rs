use grammers_client::update::Message;
use grammers_client::Client;
use log::info;

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;

pub async fn save_edited(
    message: &Message,
    client: &Client,
    client_id: u64,
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

    let diff = unified_diff(&original, &message_content);

    let user_id = message
        .sender_id()
        .map(|id| id.bare_id_unchecked())
        .unwrap_or(0);

    let chat_name = crate::utils::peer_info::chat_title(client, message).await;
    let sender_name = crate::utils::peer_info::sender_display(client, message).await;
    let sender_short: String = sender_name.chars().take(10).collect();

    if !is_log_ignored(chat_id) {
        let chat_name_short: String = chat_name.chars().take(25).collect();
        let colored = crate::utils::diff::colorize_unified_diff(&diff, &original, &message_content);
        info!(
            "\x1b[93m{:<8} {:>8} {:<25} \x1b[90m│\x1b[93m {:<10}\x1b[0m\n{}",
            "edited",
            message.id(),
            chat_name_short,
            sender_short,
            colored,
        );
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as u32;

    crate::db::EVENTS_BUF.push(Event {
        date_time: now,
        chat_id,
        chat_title: chat_name,
        message_id: msg_id,
        message: message_content,
        diff,
        user_id: user_id as u64,
        client_id,
        ..Event::edit()
    }).await;

    Ok(())
}

fn unified_diff(original: &str, modified: &str) -> String {
    similar::TextDiff::from_lines(original, modified)
        .unified_diff()
        .missing_newline_hint(false)
        .to_string()
}

