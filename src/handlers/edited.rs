use crate::render::console::{LogLine, Tone};
use grammers_client::update::Message;

use crate::app::App;
use crate::db::Event;
use crate::events::LocationEvent;

pub async fn save_edited(app: &App, message: &Message) -> anyhow::Result<()> {
    let chat_id = message.peer_id().bare_id_unchecked();
    let msg_id = message.id() as i64;
    let message_content = crate::render::format_entities::plain_text(message);
    let entities = crate::telegram::entities::of_message(message);
    let keyboard = crate::telegram::entities::keyboard_of_message(message);

    // A live location reports each move as an edit of the same message, with
    // nothing else changed. Each position is a row of its own; the edit itself
    // is then judged like any other and, with the text unchanged, skipped.
    if let Some(meta) = crate::telegram::media_description::media_meta(message)
        && meta.media_type == "live_location"
        && (meta.lat, meta.lon) != (0.0, 0.0)
    {
        let sender = crate::state::peer_info::sender_info(app, message).await;
        if !app.is_log_ignored(chat_id) {
            let chat = crate::state::peer_info::chat_info(app, message).await;
            LogLine::new(Tone::Info, "location", msg_id)
                .chat(&chat.chat_title)
                .sender(&sender.first_name)
                .body(&format!("{:.5}, {:.5}", meta.lat, meta.lon))
                .print();
        }
        app.db
            .log_event(Event::from(LocationEvent {
                date_time: edit_time(message),
                chat_id,
                message_id: msg_id,
                user_id: sender.user_id,
                lat: meta.lat,
                lon: meta.lon,
            }))
            .await;
    }

    let original = app.db.find_message(chat_id, msg_id).await;

    // What the message said before, as far as the log knows. A photo or a file
    // sent without a caption is logged as its media description, which is not
    // text the sender wrote: a caption added later replaces nothing.
    let media_desc = crate::telegram::media_description::describe(message);
    let before = if !original.logged {
        None
    } else if media_desc.as_deref() == Some(original.message.as_str()) {
        Some("")
    } else {
        Some(original.message.as_str())
    };

    // Nothing about the body changed -- Telegram also reports an edit for things
    // the log does not keep, a link preview appearing being the usual one.
    if before == Some(message_content.as_str())
        && original.entities == entities
        && original.keyboard == keyboard
    {
        return Ok(());
    }

    // A message sent before the bot saw the chat has no send row to diff
    // against: the edit is logged with the text as it now stands and an empty
    // patch, rather than one claiming every word is new.
    let keyboard_changed = original.keyboard != keyboard;
    let entities_changed = original.entities != entities;
    let original_keyboard = original.keyboard;
    let original_text = before.unwrap_or_default().to_string();
    let diff = match before {
        Some(before) => crate::render::diff::word_patch(before, &message_content),
        None => String::new(),
    };

    let sender = crate::state::peer_info::sender_info(app, message).await;

    let chat = crate::state::peer_info::chat_info(app, message).await;
    let chat_name = chat.chat_title.clone();
    let sender_name = if sender.second_name.is_empty() {
        sender.first_name.clone()
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    };
    if !app.is_log_ignored(chat_id) {
        let mut colored = crate::render::diff::inline_diff(&original_text, &message_content);
        // An edit that touches only the buttons or the formatting leaves the
        // text diff empty; say what did change instead of printing nothing.
        if keyboard_changed {
            let before = crate::render::inline_buttons::format_stored(&original_keyboard);
            let after = crate::render::inline_buttons::format_stored(&keyboard);
            let buttons = crate::render::diff::inline_diff(&before, &after);
            if !colored.is_empty() {
                colored.push('\n');
            }
            colored.push_str(&format!("buttons: {buttons}"));
        } else if entities_changed && original_text == message_content {
            colored = format!("{colored} [formatting changed]").trim().to_string();
        }
        LogLine::new(Tone::Edited, "edited", message.id())
            .chat(&chat_name)
            .sender(&sender_name)
            .body(&colored)
            .print();
    }

    // Telegram's own edit time, not the moment this process got round to it: the
    // row is when the message changed, and a reconnect replaying a backlog of
    // edits must not stamp them all with the time it caught up.
    let now = edit_time(message);

    // An edit row carries only what an edit can change: the message as it now
    // stands, the patch against what stood before -- the words that went and the
    // words that came, and nothing that stayed, which `edit_diff_html` turns back
    // into the marked-up message a board prints -- and the media the text
    // describes, plus the message object as it now stands -- an edit rewrites
    // that too, so the send row's copy is stale for an edited message.
    // Everything else is fixed when the message is sent and already on its send
    // row.
    let meta = crate::telegram::media_description::media_meta(message).unwrap_or_default();

    app.db
        .log_event(Event {
            date_time: now,
            chat_id,
            message_id: msg_id,
            message: message_content,
            entities,
            keyboard,
            diff,
            raw: serde_json::to_string(&std::ops::Deref::deref(message).raw).unwrap_or_default(),
            media_type: meta.media_type,
            file_name: meta.file_name,
            mime_type: meta.mime_type,
            size: meta.size,
            duration: meta.duration,
            width: meta.width,
            height: meta.height,
            lat: meta.lat,
            lon: meta.lon,
            poll_question: meta.poll_question,
            poll_options: meta.poll_options,
            poll_id: meta.poll_id,
            ..Event::of(crate::db::EventKind::Edit)
        })
        .await;

    Ok(())
}

/// Telegram's own edit time, or now when it gave none.
fn edit_time(message: &Message) -> u32 {
    match message.edit_date() {
        Some(date) => date.as_second() as u32,
        None => crate::db::now(),
    }
}
