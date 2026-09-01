//! Reactions on a message.
//!
//! Telegram reports them as a whole: not "someone added 👍" but "the counts are
//! now this". So a row is that snapshot, under `event = 'reaction'` — the counts
//! as they stand after the change, keyed by emoji, with a custom emoji keyed by
//! its document id.
//!
//! grammers has no friendly variant for the update yet, so it arrives as
//! `Update::Raw`, like the ephemeral ones.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;
use log::info;

use crate::db::{EVENTS_BUF, Event};
use crate::utils::log_ignore::is_log_ignored;

pub async fn save_reactions(update: &tl::types::UpdateMessageReactions) {
    // `chat_id` in `events_log` is the bare id every message path writes; the
    // dialog id is only what `peer_names` is keyed by.
    let peer = PeerId::from(&update.peer);
    let chat_id = peer.bare_id_unchecked();
    let tl::enums::MessageReactions::Reactions(reactions) = &update.reactions;

    let counts: Vec<(String, u32)> = reactions
        .results
        .iter()
        .filter_map(|result| {
            let tl::enums::ReactionCount::Count(count) = result;
            name_of(&count.reaction).map(|name| (name, count.count.max(0) as u32))
        })
        .collect();

    if !is_log_ignored(chat_id) {
        let rendered = counts
            .iter()
            .map(|(name, count)| format!("{name}×{count}"))
            .collect::<Vec<_>>()
            .join(" ");
        info!(
            "\x1b[95m{:<8} {:>8} {:<25} \x1b[90m│\x1b[95m {}\x1b[0m",
            "reaction", update.msg_id, chat_id, rendered,
        );
    }

    EVENTS_BUF
        .push(Event {
            date_time: chrono::Utc::now().timestamp() as u32,
            chat_id,
            message_id: update.msg_id as i64,
            reactions: counts,
            ..Event::reaction()
        })
        .await;
}

/// How the reaction is keyed. `Empty` is the absence of one and never appears in
/// a count; a paid reaction has no emoji of its own.
fn name_of(reaction: &tl::enums::Reaction) -> Option<String> {
    match reaction {
        tl::enums::Reaction::Emoji(e) => Some(e.emoticon.clone()),
        tl::enums::Reaction::CustomEmoji(e) => Some(e.document_id.to_string()),
        tl::enums::Reaction::Paid => Some("paid".to_string()),
        tl::enums::Reaction::Empty => None,
    }
}
