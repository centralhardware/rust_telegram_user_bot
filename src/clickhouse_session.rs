use clickhouse::Row;
use futures_core::future::BoxFuture;
use grammers_session::types::{
    ChannelKind, DcOption, PeerAuth, PeerId, PeerInfo, PeerKind, UpdateState, UpdatesState,
};
use grammers_session::Session;
use grammers_session::storages::SqliteSession;
use log::{debug, error};
use serde::{Deserialize, Serialize};

use crate::db::clickhouse;

#[derive(Row, Serialize, Deserialize)]
struct PeerRow {
    peer_id: i64,
    hash: Option<i64>,
    subtype: Option<u8>,
}

/// Session wrapper that stores peers in ClickHouse with SQLite fallback.
pub struct ClickhouseSession {
    sqlite: SqliteSession,
}

impl ClickhouseSession {
    pub async fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let sqlite = SqliteSession::open(path).await?;
        Ok(Self { sqlite })
    }

}

fn encode_subtype(peer: &PeerInfo) -> Option<u8> {
    match peer {
        PeerInfo::User { bot, is_self, .. } => {
            match (bot.unwrap_or_default(), is_self.unwrap_or_default()) {
                (true, true) => Some(3),   // UserSelfBot
                (true, false) => Some(2),  // UserBot
                (false, true) => Some(1),  // UserSelf
                (false, false) => None,
            }
        }
        PeerInfo::Chat { .. } => None,
        PeerInfo::Channel { kind, .. } => kind.map(|k| match k {
            ChannelKind::Megagroup => 4,
            ChannelKind::Broadcast => 8,
            ChannelKind::Gigagroup => 12,
        }),
    }
}

fn decode_peer(peer_id: PeerId, row: &PeerRow) -> PeerInfo {
    match peer_id.kind() {
        PeerKind::User => PeerInfo::User {
            id: peer_id.bare_id_unchecked(),
            auth: row.hash.map(PeerAuth::from_hash),
            bot: row.subtype.map(|s| s & 2 != 0),
            is_self: row.subtype.map(|s| s & 1 != 0),
        },
        PeerKind::Chat => PeerInfo::Chat {
            id: peer_id.bare_id_unchecked(),
        },
        PeerKind::Channel => PeerInfo::Channel {
            id: peer_id.bare_id_unchecked(),
            auth: row.hash.map(PeerAuth::from_hash),
            kind: row.subtype.and_then(|s| {
                if (s & 12) == 12 {
                    Some(ChannelKind::Gigagroup)
                } else if s & 8 != 0 {
                    Some(ChannelKind::Broadcast)
                } else if s & 4 != 0 {
                    Some(ChannelKind::Megagroup)
                } else {
                    None
                }
            }),
        },
    }
}

impl Session for ClickhouseSession {
    fn home_dc_id(&self) -> i32 {
        self.sqlite.home_dc_id()
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, ()> {
        self.sqlite.set_home_dc_id(dc_id)
    }

    fn dc_option(&self, dc_id: i32) -> Option<DcOption> {
        self.sqlite.dc_option(dc_id)
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, ()> {
        self.sqlite.set_dc_option(dc_option)
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Option<PeerInfo>> {
        Box::pin(async move {
            let is_self_query = peer.bot_api_dialog_id().is_none();

            if !is_self_query {
                let dialog_id = peer.bot_api_dialog_id().unwrap();
                match clickhouse()
                    .query("SELECT peer_id, hash, subtype FROM peer_cache FINAL WHERE peer_id = ?")
                    .bind(dialog_id)
                    .fetch_one::<PeerRow>()
                    .await
                {
                    Ok(row) => {
                        debug!("peer {} found in clickhouse", dialog_id);
                        return Some(decode_peer(peer, &row));
                    }
                    Err(e) => {
                        debug!("peer {} not in clickhouse: {}", dialog_id, e);
                    }
                }
            } else {
                // self_user: look for subtype with UserSelf bit set
                match clickhouse()
                    .query(
                        "SELECT peer_id, hash, subtype FROM peer_cache FINAL \
                         WHERE subtype IS NOT NULL AND bitAnd(subtype, 1) = 1 LIMIT 1",
                    )
                    .fetch_one::<PeerRow>()
                    .await
                {
                    Ok(row) => {
                        debug!("self user found in clickhouse (peer_id={})", row.peer_id);
                        let resolved = PeerId::user_unchecked(row.peer_id);
                        return Some(decode_peer(resolved, &row));
                    }
                    Err(e) => {
                        debug!("self user not in clickhouse: {}", e);
                    }
                }
            }

            // Fallback to SQLite
            self.sqlite.peer(peer).await
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, ()> {
        let peer = peer.clone();
        Box::pin(async move {
            let row = PeerRow {
                peer_id: peer.id().bot_api_dialog_id_unchecked(),
                hash: peer.auth().map(|a| a.hash()),
                subtype: encode_subtype(&peer),
            };

            // Write to ClickHouse
            match clickhouse().insert::<PeerRow>("peer_cache").await {
                Ok(mut insert) => {
                    if let Err(e) = insert.write(&row).await {
                        error!("failed to write peer {} to clickhouse: {}", row.peer_id, e);
                    } else if let Err(e) = insert.end().await {
                        error!("failed to flush peer {} to clickhouse: {}", row.peer_id, e);
                    }
                }
                Err(e) => {
                    error!("failed to insert peer {} to clickhouse: {}", row.peer_id, e);
                }
            }

            // Also write to SQLite as fallback
            self.sqlite.cache_peer(&peer).await;
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, UpdatesState> {
        self.sqlite.updates_state()
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, ()> {
        self.sqlite.set_update_state(update)
    }
}
