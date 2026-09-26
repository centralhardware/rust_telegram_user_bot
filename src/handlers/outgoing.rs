use grammers_client::peer::Peer;
use grammers_client::update::Message;

use super::extract::ChatInfo;
use super::send::Body;
use crate::app::App;
use crate::db::Event;

pub async fn save_outgoing(app: &App, message: &Message) -> anyhow::Result<Event> {
    let chat = crate::state::peer_info::chat_info(app, message).await;
    let community_id = chat.community_id;
    let (title, usernames) = (chat.chat_title, chat.chat_usernames);

    let chat_id = message.peer_id().bare_id_unchecked();

    // A chat Telegram would not name for us, and `peer_names` has no name for,
    // is still recognizable by whatever name it last went by here. Read from
    // the buffer like every other read, so a title logged a moment ago counts,
    // and with argMax rather than a sort of the chat's whole history.
    let (title, usernames) = if title.is_empty() {
        app.db
            .last_chat_name(chat_id)
            .await
            .unwrap_or((title, usernames))
    } else {
        (title, usernames)
    };

    let sender_id = message.sender_id().map(|p| p.bare_id_unchecked());
    let sender_name = message.sender().map(|p| match p {
        Peer::User(u) => u.full_name(),
        _ => p.name().unwrap_or_default().to_string(),
    });
    let body = Body::of(app, message, sender_id, sender_name.as_deref()).await;

    super::send::print(
        app,
        message,
        &body,
        ("outgoing", crate::render::console::Tone::Outgoing),
        &title,
        "",
    )
    .await;

    let chat = ChatInfo {
        chat_title: title,
        chat_usernames: usernames,
        community_id,
    };
    let event = Event {
        // The account's own message.
        user_id: app.me,
        out: true,
        ..super::send::event(app, message, &body, chat).await
    };

    app.db.log_event(event.clone()).await;

    Ok(event)
}
