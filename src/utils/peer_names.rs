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

use crate::db::{insert_rows, now};
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
    /// The ReplacingMergeTree version, and what a read orders by.
    ///
    /// The column defaults to `now()` server-side, but it is written from here
    /// anyway: rows go into a Buffer table, and a row still sitting in the
    /// buffer has to carry a version the read can order by just as much as one
    /// already merged into the table.
    pub updated_at: u32,
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
            updated_at: now(),
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
/// Read from the Buffer table (migration 041), which answers out of its own
/// memory and `peer_names` underneath both -- so a peer remembered a moment ago
/// is found here rather than sending the caller back to Telegram to resolve a
/// name already in hand.
///
/// Collapsed with `argMax` over the version column rather than with `FINAL`,
/// which is what ClickHouse recommends in place of FINAL and what this table
/// needs anyway: FINAL is passed to the destination table but is not applied to
/// the rows still in the buffer, so a peer renamed this minute would come back
/// as both its old row and its new one. Grouping by the key and taking each
/// field at the highest `updated_at` collapses the versions wherever they are,
/// buffer or table, which is what FINAL was doing here.
///
/// The version is aliased `version` rather than `updated_at`: an alias that
/// shadows the column it aggregates makes `argMax(title, updated_at)` read the
/// alias instead, and ClickHouse rejects the query as a nested aggregate.
///
/// Deliberately unmemoised: ClickHouse is the only place names live, so a
/// rename anywhere is picked up on the next lookup and nothing has to be
/// invalidated. Only the path where the update arrived without a name reaches
/// this — a named update never queries at all.
/// The peer's display name, empty when it has never been seen named.
pub async fn title_of(peer_id: i64) -> String {
    load(peer_id)
        .await
        .map(|names| names.title)
        .unwrap_or_default()
}

pub async fn load(peer_id: i64) -> Option<PeerNames> {
    match crate::db::clickhouse()
        .query(
            "SELECT peer_id, \
                    argMax(title, updated_at) AS title, \
                    argMax(first_name, updated_at) AS first_name, \
                    argMax(last_name, updated_at) AS last_name, \
                    argMax(usernames, updated_at) AS usernames, \
                    argMax(community_id, updated_at) AS community_id, \
                    max(updated_at) AS version \
             FROM peer_names_buffer WHERE peer_id = ? \
             GROUP BY peer_id",
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

/// The Buffer table in front of `peer_names` (migration 041). Written and read
/// through, so nothing about a peer is held back in the bot.
const PEER_NAMES: &str = "peer_names_buffer";

/// Store a peer's names.
///
/// Called for every named peer that passes through -- about twice per message,
/// chat and sender -- and almost always with the row already stored. Nothing
/// here filters the repeats: they go into the Buffer, which is memory, and
/// `ReplacingMergeTree` collapses them on the way down.
pub async fn remember(names: &PeerNames) {
    if let Err(e) = insert_rows(PEER_NAMES, std::slice::from_ref(names)).await {
        error!("insert into {PEER_NAMES}: {e}");
    }
}
