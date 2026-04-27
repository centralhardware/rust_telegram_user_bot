use grammers_client::update::Message;
use log::info;

use crate::db::EditedMessage;
use crate::utils::log_ignore::is_log_ignored;

pub async fn save_edited(
    message: &Message,
    client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let chat_id = message.peer_id().bare_id_unchecked();
    let msg_id = message.id() as i64;
    let mut message_content = crate::utils::format_entities::formatted_text(message);
    if let Some(b) = crate::utils::inline_buttons::format_buttons(message) {
        if !message_content.is_empty() {
            message_content.push('\n');
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
        .sender()
        .and_then(|s| s.id().bare_id())
        .unwrap_or(0) as i64;

    let chat_name = message
        .peer()
        .map(|p| p.name().unwrap_or_default().to_string())
        .unwrap_or_default();

    let sender_name = message
        .sender()
        .and_then(|p| p.name().map(|s| s.to_string()))
        .unwrap_or_default();
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

    crate::db::EDITED_BUF.push(EditedMessage {
        date_time: now,
        chat_id,
        message_id: msg_id,
        original_message: original,
        message: message_content,
        diff,
        user_id,
        client_id,
    }).await;

    Ok(())
}

fn unified_diff(original: &str, modified: &str) -> String {
    similar::TextDiff::from_lines(original, modified)
        .unified_diff()
        .missing_newline_hint(false)
        .to_string()
}

