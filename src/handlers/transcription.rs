//! The text Telegram made of a voice message.
//!
//! Transcription is asynchronous: the request returns an id, and the text
//! arrives later — first as partials, with `pending` set, then once complete.
//! Only the complete one is logged; a partial is a prefix of it and would be
//! stored as a row of its own.
//!
//! The row belongs to the voice message, and carries the text as its `message`:
//! the send row has the audio, this one has what was said.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;
use log::info;

use crate::db::{EVENTS_BUF, Event};
use crate::utils::log_ignore::is_log_ignored;
use crate::utils::peer_names::title_of;

pub async fn save_transcription(update: &tl::types::UpdateTranscribedAudio) {
    if update.pending {
        return;
    }

    let peer = PeerId::from(&update.peer);
    let chat_id = peer.bare_id_unchecked();

    if !is_log_ignored(chat_id) {
        let chat_short: String = title_of(peer.bot_api_dialog_id_unchecked())
            .await
            .chars()
            .take(25)
            .collect();
        info!(
            "\x1b[96m{:<8} {:>8} {:<25} \x1b[90m│\x1b[96m {}\x1b[0m",
            "transcr", update.msg_id, chat_short, &update.text,
        );
    }

    EVENTS_BUF
        .push(Event {
            date_time: chrono::Utc::now().timestamp() as u32,
            chat_id,
            message_id: update.msg_id as i64,
            message: update.text.clone(),
            ..Event::transcription()
        })
        .await;
}
