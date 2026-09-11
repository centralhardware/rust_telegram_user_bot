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

use log::info;

use crate::db::{EVENTS_BUF, Event};
use crate::utils::log_ignore::is_log_ignored;
use crate::utils::peer_names::title_of;

pub async fn save_pinned(chat_id: i64, dialog_id: i64, messages: &[i32], pinned: bool) {
    let date_time = chrono::Utc::now().timestamp() as u32;
    let event = if pinned { Event::pin() } else { Event::unpin() };
    let name = if pinned { "pin" } else { "unpin" };

    if !is_log_ignored(chat_id) {
        let chat_short: String = title_of(dialog_id).await.chars().take(25).collect();
        for id in messages {
            info!("\x1b[96m{:<8} {:>8} {:<25}\x1b[0m", name, id, chat_short);
        }
    }

    for &id in messages {
        EVENTS_BUF
            .push(Event {
                date_time,
                chat_id,
                message_id: id as i64,
                // The state the message is in after the update, so a row read on
                // its own says which way it went.
                pinned,
                ..event.clone()
            })
            .await;
    }
}
