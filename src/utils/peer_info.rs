use grammers_client::message::Message;
use grammers_client::peer::{Peer, User};
use grammers_client::Client;
use grammers_session::types::{PeerInfo, PeerRef};
use grammers_session::Session;
use grammers_tl_types as tl;
use std::collections::HashMap;

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

/// How many users one `users.getUsers` is asked for. Telegram's own limit is
/// higher, but a page of the history is a hundred messages, so a hundred ids is
/// already the whole page in one request.
const USERS_PER_REQUEST: usize = 100;

/// Look up the senders of a page of messages in one request each, instead of
/// leaving `sender_info` to resolve them one at a time.
///
/// `messages.Search` returns the messages of a chat but not everyone who wrote
/// them: its `users` vector leaves out the people who have since left, so their
/// messages arrive with no sender attached and every one of them used to cost a
/// `users.getUsers` of its own — thousands of extra requests over the first walk
/// of an old group, none of them paced by the backfill's request gap.
///
/// Names found here are written to `peer_names` and the peers to the session
/// cache, which is where `sender_info` looks first, so the per-message path is
/// left with the senders Telegram would not name at all.
pub async fn prefetch_senders(client: &Client, messages: &[&Message]) {
    let mut wanted: HashMap<i64, PeerRef> = HashMap::new();
    for message in messages.iter().copied() {
        // Already named on the message, or not a user: nothing to ask for.
        if message.sender().is_some() {
            continue;
        }
        let Some(sender_id) = message.sender_id() else {
            continue;
        };
        let peer_id = sender_id.bot_api_dialog_id_unchecked();
        if peer_id <= 0 || wanted.contains_key(&peer_id) {
            continue;
        }
        if let Ok(Some(peer_ref)) = message.sender_ref().await {
            wanted.insert(peer_id, peer_ref);
        }
    }
    if wanted.is_empty() {
        return;
    }

    let ids: Vec<i64> = wanted.keys().copied().collect();
    for stored in peer_names::known(&ids).await {
        wanted.remove(&stored);
    }
    if wanted.is_empty() {
        return;
    }

    let refs: Vec<PeerRef> = wanted.into_values().collect();
    for chunk in refs.chunks(USERS_PER_REQUEST) {
        let id: Vec<tl::enums::InputUser> = chunk.iter().map(|peer| (*peer).into()).collect();
        let users = match client.invoke(&tl::functions::users::GetUsers { id }).await {
            Ok(users) => users,
            Err(e) => {
                log::warn!("looking up {} senders at once: {e}", chunk.len());
                return;
            }
        };
        let mut peers = Vec::with_capacity(users.len());
        for raw in users {
            let peer = Peer::User(User::from_raw(client, raw));
            if let Some(names) = PeerNames::from_peer(&peer) {
                peer_names::remember(&names).await;
            }
            peers.push(PeerInfo::from(peer));
        }
        // The access hashes as well: resolving a peer by hand skips the caching
        // grammers does inside `resolve_peer`, and without it the next message
        // from the same sender has nothing to reference them by.
        if let Some(session) = crate::session::session() {
            if let Err(e) = session.cache_peers(peers).await {
                log::warn!("caching the senders just looked up: {e}");
            }
        }
    }
}
