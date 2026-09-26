use grammers_client::message::Message;

use crate::app::App;
use crate::handlers::extract::{ChatInfo, SenderInfo};
use crate::state::peer_names::{self, PeerNames};

// Updates only carry the peers Telegram bothered to attach, so a message can
// arrive with neither its chat nor its sender in the in-memory peer map. Names
// for those are looked up in ClickHouse's `peer_names`, filled by every peer
// that does come through named; a peer that has never been seen named stays
// nameless rather than being resolved against Telegram, which costs a round
// trip per message and, on a backfill, enough of them to be rate-limited.

/// The message's chat, named from `peer_names` when the update did not carry
/// it. Falls back to whatever the update did have (usually nothing).
pub async fn chat_info(app: &App, message: &Message) -> ChatInfo {
    if let Some(peer) = message.peer()
        && let Some(names) = PeerNames::from_peer(peer) {
            peer_names::remember(app, &names).await;
            return names.chat_info();
        }

    let peer_id = message.peer_id().bot_api_dialog_id_unchecked();
    peer_names::load(app, peer_id)
        .await
        .map(|names| names.chat_info())
        .unwrap_or_default()
}

/// The message's sender, named from `peer_names` when the update did not carry
/// it. Empty when the message has no sender at all (channel posts).
///
/// Only the three name columns are ever missing — the id comes off the message
/// itself, so an unnamed sender is still logged with its author.
pub async fn sender_info(app: &App, message: &Message) -> SenderInfo {
    if let Some(peer) = message.sender()
        && let Some(names) = PeerNames::from_peer(peer) {
            peer_names::remember(app, &names).await;
            if let Some(sender) = names.sender_info() {
                return sender;
            }
        }

    let sender_id = match message.sender_id() {
        Some(id) => id.bot_api_dialog_id_unchecked(),
        None => return SenderInfo::default(),
    };
    // Groups and channels have no sender identity; `sender_info` rejects them.
    if sender_id <= 0 {
        return SenderInfo::default();
    }

    peer_names::load(app, sender_id)
        .await
        .and_then(|names| names.sender_info())
        .unwrap_or(SenderInfo {
            user_id: sender_id as u64,
            ..SenderInfo::default()
        })
}
