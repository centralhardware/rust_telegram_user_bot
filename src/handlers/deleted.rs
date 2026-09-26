use crate::render::console::{LogLine, Tone};
use grammers_client::update::MessageDeletion;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::App;
use crate::db::Event;
use crate::events::DeleteEvent;

pub async fn save_deleted(app: &App, deletion: &MessageDeletion) -> anyhow::Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as u32;
    let ids: Vec<i64> = deletion.messages().iter().map(|&id| id as i64).collect();
    let channel = deletion.channel_id();

    // One read for the whole deletion. Telegram only names the chat for a
    // channel or a supergroup; for a private chat or a basic group the log has
    // to say where each message was.
    // A channel that is log-ignored needs no lookup: its rows are written
    // anyway, and nothing is printed.
    let found: HashMap<i64, _> = match channel {
        Some(chat_id) if app.is_log_ignored(chat_id) => HashMap::new(),
        _ => app
            .db
            .find_deleted(channel, &ids)
            .await
            .into_iter()
            .map(|m| (m.message_id, m))
            .collect(),
    };

    let mut rows = Vec::with_capacity(ids.len());
    for msg_id in ids {
        let m = found.get(&msg_id);
        let Some(chat_id) = channel.or(m.map(|m| m.chat_id)) else {
            continue;
        };

        if !app.is_log_ignored(chat_id) {
            let (message, sender, title) = m.map_or(("", "", ""), |m| {
                (
                    m.message.as_str(),
                    m.first_name.as_str(),
                    m.chat_title.as_str(),
                )
            });
            let title = if title.is_empty() {
                chat_id.to_string()
            } else {
                title.to_string()
            };
            LogLine::new(Tone::Deleted, "deleted", msg_id)
                .chat(&title)
                .sender(sender)
                .body(message)
                .print();
        }

        // Telegram names nothing but the chat and the id, and that is all the
        // row keeps: what the message was is already on its send row.
        rows.push(Event::from(DeleteEvent {
            date_time: now,
            chat_id,
            message_id: msg_id,
            chat_title: String::new(),
            ephemeral: false,
        }));
    }

    // One insert for the whole deletion.
    if !rows.is_empty() {
        app.db.log_events(&rows).await;
    }

    Ok(())
}
