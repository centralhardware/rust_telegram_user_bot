use grammers_client::message::Message;
use grammers_client::Client;
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::handlers::extract::{chat_from_peer, sender_from_peer, ChatInfo, SenderInfo};

/// Updates only carry the peers Telegram bothered to attach, so a message can
/// arrive with neither its chat nor its sender in the in-memory peer map — the
/// chat then gets logged and stored without a name. Names change rarely, so one
/// resolve per peer per process is enough to fill those gaps.
static CHATS: LazyLock<Mutex<HashMap<i64, ChatInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SENDERS: LazyLock<Mutex<HashMap<i64, SenderInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The message's chat, resolving it against Telegram when the update did not
/// carry it. Falls back to whatever the update did have (usually nothing).
pub async fn chat_info(client: &Client, message: &Message) -> ChatInfo {
    let from_update = crate::handlers::extract::extract_chat(message);
    if !from_update.chat_title.is_empty() {
        return from_update;
    }

    let chat_id = message.peer_id().bare_id_unchecked();
    if let Some(cached) = CHATS.lock().await.get(&chat_id) {
        return cached.clone();
    }

    let peer = match message.peer_ref().await {
        Ok(Some(peer)) => peer,
        _ => return from_update,
    };
    let resolved = match client.resolve_peer(peer).await {
        Ok(peer) => chat_from_peer(&peer),
        Err(e) => {
            log::warn!("resolving chat {chat_id}: {e}");
            return from_update;
        }
    };

    // Never cache a blank name: a peer that could not be named this time must
    // stay resolvable later, not be pinned empty for the process's lifetime.
    if resolved.chat_title.is_empty() {
        return from_update;
    }
    CHATS.lock().await.insert(chat_id, resolved.clone());
    resolved
}

/// The message's sender, resolving it against Telegram when the update did not
/// carry it. Empty when the message has no sender at all (channel posts).
pub async fn sender_info(client: &Client, message: &Message) -> SenderInfo {
    let from_update = crate::handlers::extract::extract_sender(message);
    if from_update.user_id != 0 {
        return from_update;
    }

    let sender_id = match message.sender_id() {
        Some(id) => id.bare_id_unchecked(),
        None => return from_update,
    };
    if let Some(cached) = SENDERS.lock().await.get(&sender_id) {
        return cached.clone();
    }

    let peer = match message.sender_ref().await {
        Ok(Some(peer)) => peer,
        _ => return from_update,
    };
    let resolved = match client.resolve_peer(peer).await {
        Ok(peer) => sender_from_peer(&peer),
        Err(e) => {
            log::warn!("resolving sender {sender_id}: {e}");
            return from_update;
        }
    };

    if resolved.user_id == 0 {
        return from_update;
    }
    SENDERS.lock().await.insert(sender_id, resolved.clone());
    resolved
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
