//! Persistent display names for peers, in ClickHouse's `peer_names`.
//!
//! Kept out of `peer_cache` on purpose: that table is the grammers session
//! store, written from `cache_peer`, which only ever sees a `PeerInfo` — id,
//! auth hash and subtype, no names. Names are only available on a resolved
//! `Peer`, so they have to be written from a different path, and a partial row
//! into a ReplacingMergeTree would blank the access hash the session needs.

use clickhouse::Row;
use grammers_client::peer::Peer;
use log::{debug, error};
use serde::{Deserialize, Serialize};

use crate::handlers::extract::{ChatInfo, SenderInfo};

/// The community a chat belongs to. Only a channel or a supergroup can be in
/// one, and Telegram reports it on the chat rather than on its messages.
fn community_of(peer: &Peer) -> i64 {
    let channel = match peer {
        Peer::Channel(channel) => &channel.raw,
        Peer::Group(group) => match &group.raw {
            grammers_tl_types::enums::Chat::Channel(channel) => channel,
            _ => return 0,
        },
        _ => return 0,
    };
    channel.linked_community_id.unwrap_or(0)
}

#[derive(Row, Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct PeerNames {
    /// Bot API dialog id: users positive, legacy groups `-id`, channels `-100…`.
    pub peer_id: i64,
    /// Display form — a chat/channel title, or "First Last" for a user.
    pub title: String,
    pub first_name: String,
    pub last_name: String,
    pub usernames: Vec<String>,
    /// The community the chat belongs to, 0 when it belongs to none. A property
    /// of the chat rather than of the message, which is why it is remembered
    /// here with the chat's other identity.
    pub community_id: i64,
}

impl PeerNames {
    /// The names carried by an already-resolved peer, or `None` when it carries
    /// none — a peer that could not be named must stay resolvable later rather
    /// than be pinned blank.
    pub fn from_peer(peer: &Peer) -> Option<Self> {
        let (first_name, last_name) = match peer {
            Peer::User(user) => (
                user.first_name().unwrap_or_default().to_string(),
                user.last_name().unwrap_or_default().to_string(),
            ),
            _ => (String::new(), String::new()),
        };

        let title = match peer {
            Peer::User(_) => {
                if last_name.is_empty() {
                    first_name.clone()
                } else {
                    format!("{first_name} {last_name}")
                }
            }
            _ => peer.name().unwrap_or_default().to_string(),
        };

        if title.is_empty() {
            return None;
        }

        // `usernames()` only carries *collectible* usernames and is empty for
        // the ordinary case of a peer with a single @name, so the primary one
        // has to be put in front of it by hand.
        let mut usernames: Vec<String> = peer.username().map(str::to_string).into_iter().collect();
        for extra in peer.usernames() {
            if !usernames.iter().any(|u| u == extra) {
                usernames.push(extra.to_string());
            }
        }

        Some(Self {
            peer_id: peer.id().bot_api_dialog_id_unchecked(),
            title,
            first_name,
            last_name,
            usernames,
            community_id: community_of(peer),
        })
    }

    pub fn chat_info(&self) -> ChatInfo {
        ChatInfo {
            chat_title: self.title.clone(),
            chat_usernames: self.usernames.clone(),
            community_id: self.community_id,
        }
    }

    /// Only a user has a sender identity — for them the Bot API dialog id is
    /// the bare id the logs store. `None` for groups and channels.
    pub fn sender_info(&self) -> Option<SenderInfo> {
        if self.peer_id <= 0 {
            return None;
        }
        Some(SenderInfo {
            username: vec![self.usernames.first().cloned().unwrap_or_default()],
            first_name: self.first_name.clone(),
            second_name: self.last_name.clone(),
            user_id: self.peer_id as u64,
        })
    }
}

/// The stored names for a peer, or `None` when it has never been seen.
///
/// Deliberately unmemoised: ClickHouse is the only place names live, so a
/// rename anywhere is picked up on the next lookup and nothing has to be
/// invalidated. Only the path where the update arrived without a name reaches
/// this — a named update never queries at all.
pub async fn load(peer_id: i64) -> Option<PeerNames> {
    match crate::db::clickhouse()
        .query(
            "SELECT peer_id, title, first_name, last_name, usernames, community_id \
             FROM peer_names FINAL WHERE peer_id = ?",
        )
        .bind(peer_id)
        .fetch_one::<PeerNames>()
        .await
    {
        Ok(row) => Some(row),
        Err(clickhouse::error::Error::RowNotFound) => {
            debug!("peer {peer_id} has no stored names");
            None
        }
        Err(e) => {
            error!("looking up names for peer {peer_id}: {e}");
            None
        }
    }
}

/// Store a peer's names.
///
/// Called for every named peer that passes through, so this runs about once per
/// message: the insert is async and unwaited, and `ReplacingMergeTree` collapses
/// the repeats on merge, which is what keeps that affordable.
pub async fn remember(names: &PeerNames) {
    match crate::db::clickhouse_async_insert()
        .insert::<PeerNames>("peer_names")
        .await
    {
        Ok(mut insert) => {
            if let Err(e) = insert.write(names).await {
                error!("failed to write names for peer {}: {e}", names.peer_id);
            } else if let Err(e) = insert.end().await {
                error!("failed to flush names for peer {}: {e}", names.peer_id);
            }
        }
        Err(e) => error!("failed to insert names for peer {}: {e}", names.peer_id),
    }
}
