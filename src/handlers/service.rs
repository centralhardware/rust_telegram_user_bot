use grammers_client::peer::Peer;
use grammers_client::update::Message;
use crate::render::console::{LogLine, Tone};

use crate::db::Event;
use crate::events::ServiceEvent;
use crate::app::App;

/// A service message that is nothing but a mark on another message — a pin — is
/// logged as an event *of that message*, not as a message of its own.
///
/// Telegram delivers it as a message with an id and a sender, but all it says is
/// "this happened to that one". Stored as itself it was a row whose text was a
/// sentence about a message living elsewhere, while the message it marks kept no
/// trace of the pin at all. So the target is backfilled if the log has never seen
/// it (`backfill_reply` has already run by then, off the same reply header) and
/// the action is written onto the target's id: `chat_id`, `message_id`, `action`
/// and the announcement's own id in `service_message_id` — nothing else, close to
/// the way a delete keeps nothing but the id it names. A
/// message's history then reads as its own rows — send, edit, pin, delete.
///
/// Returns whether it took the message. A service message that carries its own
/// meaning — a title change, a join, a call — is left to the ordinary save.
pub async fn save_service(app: &App, message: &Message) -> bool {
    let Some(action) = message.action() else {
        return false;
    };
    let Some(target) = crate::telegram::service_action::target(message, action) else {
        return false;
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    let kind = crate::telegram::service_action::kind(action);

    app.db.log_event(Event::from(ServiceEvent {
        date_time: message.date().as_second() as u32,
        chat_id,
        message_id: target as i64,
        // The announcement's own id: the row is keyed on the message the
        // action was performed on, so this is the only place it fits.
        service_message_id: message.id() as i64,
        action: kind.clone(),
    }))
    .await;

    if !app.is_log_ignored(chat_id) {
        let chat = crate::state::peer_info::chat_info(app, message).await;
        let sender = message
            .sender()
            .map(|p| match p {
                Peer::User(u) => u.full_name(),
                _ => p.name().unwrap_or_default().to_string(),
            })
            .unwrap_or_default();
        LogLine::new(Tone::Action, "service", target)
            .chat(&chat.chat_title)
            .sender(&sender)
            .body(&kind)
            .print();
    }

    true
}
