//! Ephemeral messages: a bot's reply inside a group that only one member can
//! see, and the ephemeral commands sent to it, which the rest of the group does
//! not see either (Bot API 10.2, layer 228).
//!
//! They never join the chat history, so they arrive on updates of their own —
//! `updateNewEphemeralMessage` and friends — which grammers has no friendly
//! variant for yet and hands over as `Update::Raw`. Without this handler the
//! account sees them and the log does not.
//!
//! Their ids are a separate sequence from the chat's, so they get their own
//! table rather than a corner of `events_log`; see `018_create_ephemeral_log.sql`.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;
use log::info;

use crate::db::{EPHEMERAL_BUF, EphemeralEvent};
use crate::utils::log_ignore::is_log_ignored;
use crate::utils::peer_names;

/// A new or edited ephemeral message. `event` is the column the two share a
/// table under: `"new"` or `"edit"`.
pub async fn save_ephemeral(message: &tl::enums::EphemeralMessage, event: &str, client_id: u64) {
    let tl::enums::EphemeralMessage::Message(msg) = message;

    // A bot can also send one outside a group, straight to the receiver: then
    // there is no group peer and the bot itself is the chat.
    let chat_id = PeerId::from(msg.peer_id.as_ref().unwrap_or(&msg.from_id))
        .bot_api_dialog_id_unchecked();
    let sender = PeerId::from(&msg.from_id);

    let chat_title = title_of(chat_id).await;
    let sender_title = title_of(sender.bot_api_dialog_id_unchecked()).await;

    let text = body(msg);
    let (reply_to, reply_to_ephemeral) = reply(msg);

    if !is_log_ignored(chat_id) {
        let sender_short: String = sender_title.chars().take(10).collect();
        let chat_short: String = chat_title.chars().take(25).collect();
        info!(
            "\x1b[94m{:<8} {:>8} {:<25} \x1b[90m│\x1b[94m {:<10} \x1b[90m│\x1b[94m {}\x1b[0m",
            format!("eph {event}"),
            msg.id,
            chat_short,
            sender_short,
            &text
        );
    }

    EPHEMERAL_BUF
        .push(EphemeralEvent {
            date_time: msg.date as u32,
            event: event.to_string(),
            chat_id,
            chat_title,
            message_id: msg.id as i64,
            message: text,
            sender_id: sender.bare_id_unchecked() as u64,
            sender_title,
            out: msg.out,
            receiver_id: msg.receiver_id as u64,
            top_msg_id: msg.top_msg_id.unwrap_or(0) as u32,
            reply_to,
            reply_to_ephemeral,
            welcome: msg.welcome_template,
            client_id,
        })
        .await;
}

/// Ephemeral messages are deleted by id alone: Telegram names the chat and the
/// ids, and nothing about what was in them.
pub async fn save_ephemeral_deleted(peer: &tl::enums::Peer, ids: &[i32], client_id: u64) {
    let chat_id = PeerId::from(peer).bot_api_dialog_id_unchecked();
    let chat_title = title_of(chat_id).await;
    let date_time = chrono::Utc::now().timestamp() as u32;

    if !is_log_ignored(chat_id) {
        let chat_short: String = chat_title.chars().take(25).collect();
        for id in ids {
            info!(
                "\x1b[94m{:<8} {:>8} {:<25}\x1b[0m",
                "eph del", id, chat_short
            );
        }
    }

    for id in ids {
        EPHEMERAL_BUF
            .push(EphemeralEvent {
                date_time,
                event: "delete".to_string(),
                chat_id,
                chat_title: chat_title.clone(),
                message_id: *id as i64,
                message: String::new(),
                sender_id: 0,
                sender_title: String::new(),
                out: false,
                receiver_id: 0,
                top_msg_id: 0,
                reply_to: 0,
                reply_to_ephemeral: false,
                welcome: false,
                client_id,
            })
            .await;
    }
}

/// The message body as the log stores it: rich payload if there is one, else the
/// text with its entities, with the media description and the buttons around it
/// exactly as an ordinary message gets them.
fn body(msg: &tl::types::EphemeralMessage) -> String {
    let rich = msg.rich_message.as_ref().and_then(crate::utils::rich_message::render);
    let text = match rich {
        Some(rich) => rich,
        None => crate::utils::format_entities::render(&msg.message, msg.entities.as_deref()),
    };

    let media = msg
        .media
        .as_ref()
        .map(crate::utils::media_description::describe_media);

    let mut out = match (media, text.is_empty()) {
        (Some(media), false) => format!("{media} {text}"),
        (Some(media), true) => media,
        (None, _) => text,
    };

    let buttons = msg
        .reply_markup
        .as_ref()
        .and_then(crate::utils::inline_buttons::format_markup);
    if let Some(buttons) = buttons {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&buttons);
    }
    out
}

/// What the message replies to, and whether that id is itself an ephemeral one —
/// a bot can reply to its own ephemeral message as well as to a real one, and the
/// two ids come from different sequences.
fn reply(msg: &tl::types::EphemeralMessage) -> (u64, bool) {
    let Some(tl::enums::MessageReplyHeader::Header(header)) = msg.reply_to.as_ref() else {
        return (0, false);
    };
    (
        header.reply_to_msg_id.unwrap_or(0) as u64,
        header.reply_to_ephemeral,
    )
}

/// The stored name for a peer. Ephemeral updates carry no peer objects at all,
/// so there is nothing to resolve from and nothing to write back: an unknown
/// peer stays blank until ordinary traffic in that chat names it.
async fn title_of(peer_id: i64) -> String {
    peer_names::load(peer_id)
        .await
        .map(|names| names.title)
        .unwrap_or_default()
}
