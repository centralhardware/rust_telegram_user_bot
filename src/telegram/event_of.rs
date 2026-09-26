//! Turn a message fetched from Telegram into the `events_log` row it would have
//! got had it arrived as an update.
//!
//! A message the account was not listening for — the target of a reply, or one
//! pulled out of a chat's history by a backfill — has to be logged with the same
//! columns a live one gets, or the row is quietly the thinner of the two. Both
//! callers build it here so there is one answer to what a row holds.

use grammers_client::message::Message;

use crate::app::App;
use crate::db::Event;
use crate::handlers::extract::extract_community_tag;
use crate::state::peer_info::{chat_info, sender_info};

pub async fn event_of(app: &App, msg: &Message) -> Event {
    let chat_id = msg.peer_id().bare_id_unchecked();

    let sender = sender_info(app, msg).await;
    let chat = chat_info(app, msg).await;

    let text = crate::render::format_entities::plain_text(msg);
    let action_desc = match msg.action() {
        Some(action) if text.is_empty() => {
            let sender_display = if sender.second_name.is_empty() {
                sender.first_name.clone()
            } else {
                format!("{} {}", sender.first_name, sender.second_name)
            };
            let game_title = crate::telegram::service_action::game_title(&app.tg, msg).await;
            Some(crate::telegram::service_action::format(
                action,
                Some(sender.user_id as i64),
                Some(&sender_display),
                game_title.as_deref(),
            ))
        }
        _ => None,
    };

    let mut reply = crate::telegram::reply_target::reply_info(msg);
    let reply_to_user_id = crate::db::resolve_reply(&*app.db, chat_id, &mut reply).await;
    let (topic_id, topic_name) = crate::state::topic::topic_of(app, msg).await;
    let raw = serde_json::to_string(&msg.raw).unwrap_or_default();

    let mut event = Event {
        username: sender.username,
        first_name: sender.first_name,
        second_name: sender.second_name,
        user_id: sender.user_id,
        community_tag: extract_community_tag(&msg.raw),
        // A fetched message can be one this account sent: a send row defaults
        // to incoming, which would be wrong for half of them.
        out: crate::telegram::self_id::is_outgoing(app.me, msg),
        ..crate::telegram::event_row::build(
            &msg.raw,
            crate::telegram::event_row::Context {
                chat,
                reply,
                reply_to_user_id,
                topic_id,
                topic_name,
                action_desc,
                raw: raw.clone(),
            },
        )
    };
    // A message that is neither text, an action nor media stands in for itself.
    if event.message.is_empty() {
        event.message = raw;
    }
    event
}
