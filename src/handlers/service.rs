use grammers_client::peer::Peer;
use grammers_client::update::Message;
use grammers_client::Client;
use log::info;

use crate::db::Event;
use crate::utils::log_ignore::is_log_ignored;

/// A service message that is nothing but a mark on another message — a pin — is
/// logged as an event *of that message*, not as a message of its own.
///
/// Telegram delivers it as a message with an id and a sender, but all it says is
/// "this happened to that one". Stored as itself it was a row whose text was a
/// sentence about a message living elsewhere, while the message it marks kept no
/// trace of the pin at all. So the target is backfilled if the log has never seen
/// it (`backfill_reply` has already run by then, off the same reply header) and
/// the action is written onto the target's id: `chat_id`, `message_id`, `action`
/// and nothing else, the way a delete keeps nothing but the id it names. A
/// message's history then reads as its own rows — send, edit, pin, delete.
///
/// Returns whether it took the message. A service message that carries its own
/// meaning — a title change, a join, a call — is left to the ordinary save.
pub async fn save_service(client: &Client, message: &Message) -> bool {
    let Some(action) = message.action() else {
        return false;
    };
    let Some(target) = crate::utils::service_action::target(message, action) else {
        return false;
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    let kind = crate::utils::service_action::kind(action);

    crate::db::EVENTS_BUF
        .push(Event {
            date_time: message.date().as_second() as u32,
            chat_id,
            message_id: target as i64,
            action: kind.clone(),
            ..Event::service()
        })
        .await;

    if !is_log_ignored(chat_id) {
        let chat = crate::utils::peer_info::chat_info(client, message).await;
        let chat_short: String = chat.chat_title.chars().take(25).collect();
        let sender_short: String = message
            .sender()
            .map(|p| match p {
                Peer::User(u) => u.full_name(),
                _ => p.name().unwrap_or_default().to_string(),
            })
            .unwrap_or_default()
            .chars()
            .take(10)
            .collect();
        info!(
            "\x1b[95m{:<8} {:>8} {:<25} \x1b[90m│\x1b[95m {:<10} \x1b[90m│\x1b[95m {}\x1b[0m",
            "service", target, chat_short, sender_short, kind
        );
    }

    true
}
