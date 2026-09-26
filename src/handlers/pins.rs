//! A message pinned or unpinned.
//!
//! In a group a pin also arrives as a service message, which `service.rs` logs
//! against the message it names. Nothing announces an *un*pin, and neither a
//! channel nor a private chat announces a pin at all — Telegram only says, out
//! of band, "these ids are pinned now" or "these are not". So this is the row
//! that records it: `pin` / `unpin`, against the message itself, one per id.
//!
//! grammers has no friendly variant for the update, so it arrives as
//! `Update::Raw`, like the ephemeral ones.

use crate::render::console::{LogLine, Tone};

use crate::app::App;
use crate::db::Event;
use crate::events::PinEvent;
use crate::state::peer_names::title_of;

pub async fn save_pinned(app: &App, chat_id: i64, dialog_id: i64, messages: &[i32], pinned: bool) {
    let date_time = chrono::Utc::now().timestamp() as u32;
    let name = if pinned { "pin" } else { "unpin" };

    if !app.is_log_ignored(chat_id) {
        let chat = title_of(app, dialog_id).await;
        for id in messages {
            LogLine::new(Tone::Info, name, id).chat(&chat).print();
        }
    }

    for &id in messages {
        app.db
            .log_event(Event::from(PinEvent {
                date_time,
                chat_id,
                message_id: id as i64,
                // The state the message is in after the update, so a row read on
                // its own says which way it went.
                pinned,
            }))
            .await;
    }
}
