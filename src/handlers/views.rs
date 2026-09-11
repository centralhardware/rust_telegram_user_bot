//! How often a channel post was seen or forwarded.
//!
//! Telegram reports each counter as its own update, and reports it as a total
//! rather than a delta — so a row is the count as it stands, the way a reaction
//! row is. The two updates never arrive together, so the counter the update did
//! not carry stays 0: read a post's latest `views` row for views and its latest
//! `forwards` row for forwards rather than expecting one row to hold both.

use log::info;

use crate::db::{EVENTS_BUF, Event};
use crate::utils::log_ignore::is_log_ignored;
use crate::utils::peer_names::title_of;

pub async fn save_views(channel_id: i64, message_id: i32, views: u32, forwards: u32) {
    if !is_log_ignored(channel_id) {
        let dialog_id = -1_000_000_000_000 - channel_id;
        let chat_short: String = title_of(dialog_id).await.chars().take(25).collect();
        let rendered = if forwards > 0 {
            format!("{forwards} forwards")
        } else {
            format!("{views} views")
        };
        info!(
            "\x1b[96m{:<8} {:>8} {:<25} \x1b[90m│\x1b[96m {}\x1b[0m",
            "views", message_id, chat_short, rendered,
        );
    }

    EVENTS_BUF
        .push(Event {
            date_time: chrono::Utc::now().timestamp() as u32,
            chat_id: channel_id,
            message_id: message_id as i64,
            views,
            forwards,
            ..Event::views()
        })
        .await;
}
