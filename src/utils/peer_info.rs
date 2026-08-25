use grammers_client::message::Message;
use grammers_client::Client;

use crate::handlers::extract::{ChatInfo, SenderInfo};
use crate::utils::peer_names::{self, PeerNames};

/// Updates only carry the peers Telegram bothered to attach, so a message can
/// arrive with neither its chat nor its sender in the in-memory peer map — the
/// chat then gets logged and stored without a name. Names are looked up in
/// ClickHouse's `peer_names` (memoised per process by that module) and only
/// resolved against Telegram when they are not stored yet; every peer that does
/// come through named is written back, so the table fills itself.

/// The message's chat, resolving it against Telegram when the update did not
/// carry it. Falls back to whatever the update did have (usually nothing).
pub async fn chat_info(client: &Client, message: &Message) -> ChatInfo {
    let from_update = match message.peer() {
        Some(peer) => match PeerNames::from_peer(peer) {
            Some(names) => {
                peer_names::remember(&names).await;
                return names.chat_info();
            }
            None => ChatInfo::default(),
        },
        None => ChatInfo::default(),
    };

    let peer_id = message.peer_id().bot_api_dialog_id_unchecked();
    match resolve(client, message, peer_id, Target::Chat).await {
        Some(names) => names.chat_info(),
        None => from_update,
    }
}

/// The message's sender, resolving it against Telegram when the update did not
/// carry it. Empty when the message has no sender at all (channel posts).
pub async fn sender_info(client: &Client, message: &Message) -> SenderInfo {
    if let Some(peer) = message.sender() {
        if let Some(names) = PeerNames::from_peer(peer) {
            peer_names::remember(&names).await;
            if let Some(sender) = names.sender_info() {
                return sender;
            }
        }
    }

    let sender_id = match message.sender_id() {
        Some(id) => id.bot_api_dialog_id_unchecked(),
        None => return SenderInfo::default(),
    };
    resolve(client, message, sender_id, Target::Sender)
        .await
        .and_then(|names| names.sender_info())
        .unwrap_or_default()
}

enum Target {
    Chat,
    Sender,
}

/// Stored names for the peer, falling back to one resolve against Telegram.
async fn resolve(
    client: &Client,
    message: &Message,
    peer_id: i64,
    target: Target,
) -> Option<PeerNames> {
    if let Some(stored) = peer_names::load(peer_id).await {
        return Some(stored);
    }

    let peer_ref = match &target {
        Target::Chat => message.peer_ref().await,
        Target::Sender => message.sender_ref().await,
    };
    let peer_ref = match peer_ref {
        Ok(Some(peer_ref)) => peer_ref,
        _ => return None,
    };

    let peer = match client.resolve_peer(peer_ref).await {
        Ok(peer) => peer,
        Err(e) => {
            log::warn!("resolving peer {peer_id}: {e}");
            return None;
        }
    };

    // Never store a blank name: a peer that could not be named this time must
    // stay resolvable later, not be pinned empty.
    let names = PeerNames::from_peer(&peer)?;
    peer_names::remember(&names).await;
    Some(names)
}

/// The chat's display name, for handlers that only log a name.
pub async fn chat_title(client: &Client, message: &Message) -> String {
    chat_info(client, message).await.chat_title
}

/// The sender's display name ("First Last"), for handlers that only log a name.
pub async fn sender_display(client: &Client, message: &Message) -> String {
    let sender = sender_info(client, message).await;
    if sender.second_name.is_empty() {
        sender.first_name
    } else {
        format!("{} {}", sender.first_name, sender.second_name)
    }
}
