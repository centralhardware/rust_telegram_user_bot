//! How often a channel post was seen or forwarded.
//!
//! Telegram reports each counter as its own update, and reports it as a total
//! rather than a delta — so a row is the count as it stands, the way a reaction
//! row is. The two updates never arrive together, so the counter the update did
//! not carry stays 0: read a post's latest `views` row for views and its latest
//! `forwards` row for forwards rather than expecting one row to hold both.

use crate::render::console::{LogLine, Tone};

use crate::app::App;
use crate::db::Event;
use crate::events::ViewsEvent;
use crate::state::peer_names::title_of;

pub async fn save_views(app: &App, channel_id: i64, message_id: i32, views: u32, forwards: u32) {
    if !app.is_log_ignored(channel_id) {
        // The post itself: an update names nothing but the counter, so what the
        // counter is counting is read back from the message's send row.
        let post = app.db.find_message(channel_id, message_id as i64).await;
        let dialog_id = -1_000_000_000_000 - channel_id;
        let title = match title_of(app, dialog_id).await {
            t if !t.is_empty() => t,
            _ if !post.chat_title.is_empty() => post.chat_title,
            _ => channel_id.to_string(),
        };
        let counter = if forwards > 0 {
            format!("{forwards} forwards")
        } else {
            format!("{views} views")
        };
        let text: String = post.message.replace('\n', " ").chars().take(60).collect();
        let rendered = if text.is_empty() {
            counter
        } else {
            format!("{counter} — {text}")
        };
        LogLine::new(Tone::Info, "views", message_id)
            .chat(&title)
            .body(&rendered)
            .print();
    }

    app.db
        .log_event(Event::from(ViewsEvent {
            date_time: chrono::Utc::now().timestamp() as u32,
            chat_id: channel_id,
            message_id: message_id as i64,
            views,
            forwards,
        }))
        .await;
}
