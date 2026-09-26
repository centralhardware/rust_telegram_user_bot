use grammers_client::update::Message;
use crate::render::console::{LogLine, Tone};

use crate::db::Event;
use crate::app::App;

pub async fn save_edited(app: &App, message: &Message) -> anyhow::Result<()> {
    let chat_id = message.peer_id().bare_id_unchecked();
    let msg_id = message.id() as i64;
    let message_content = crate::render::format_entities::plain_text(message);
    let entities = crate::telegram::entities::of_message(message);
    let keyboard = crate::telegram::entities::keyboard_of_message(message);

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
    let original = before.unwrap_or_default().to_string();
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
        let colored = crate::render::diff::inline_diff(&original, &message_content);
        LogLine::new(Tone::Edited, "edited", message.id())
            .chat(&chat_name)
            .sender(&sender_name)
            .body(&colored)
            .print();
    }

    // Telegram's own edit time, not the moment this process got round to it: the
    // row is when the message changed, and a reconnect replaying a backlog of
    // edits must not stamp them all with the time it caught up.
    let now = match message.edit_date() {
        Some(date) => date.as_second() as u32,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as u32,
    };

    // An edit row carries only what an edit can change: the message as it now
    // stands, the patch against what stood before -- the words that went and the
    // words that came, and nothing that stayed, which `edit_diff_html` turns back
    // into the marked-up message a board prints -- and the media the text
    // describes, plus the message object as it now stands -- an edit rewrites
    // that too, so the send row's copy is stale for an edited message.
    // Everything else is fixed when the message is sent and already on its send
    // row.
    let meta = crate::telegram::media_description::media_meta(message).unwrap_or_default();

    app.db.log_event(Event {
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
    }).await;

    Ok(())
}
