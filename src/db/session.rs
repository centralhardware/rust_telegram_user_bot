//! The grammers session store, kept in the `session_*` and `peer_cache` tables:
//! the home DC, DC keys, update positions and every peer seen. Infrastructure
//! for the Telegram client rather than something a handler asks for, so it
//! uses the ClickHouse client directly instead of going through [`Db`](super::Db).

use std::sync::PoisonError;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use clickhouse::Row;
use futures_core::future::BoxFuture;
use grammers_session::types::{
    ChannelKind, ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, PeerKind, UpdateState,
    UpdatesState,
};
use grammers_session::{Session, SessionData};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};

use crate::db::ch::insert_rows;
use crate::db::now;

// ── ClickHouse row types ────────────────────────────────────────────

#[derive(Row, Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct PeerRow {
    peer_id: i64,
    hash: Option<i64>,
    subtype: Option<u8>,
    /// The ReplacingMergeTree version, and what a read orders by.
    ///
    /// The column defaults to `now()` server-side, but it is written from here
    /// anyway: rows go into a Buffer table, and a row still sitting in the
    /// buffer has to carry a version the read can order by just as much as one
    /// already merged into the table.
    updated_at: u32,
}

#[derive(Row, Serialize, Deserialize)]
struct DcHomeRow {
    dc_id: i32,
}

#[derive(Row, Serialize, Deserialize)]
struct DcOptionRow {
    dc_id: i32,
    ipv4: String,
    ipv6: String,
    auth_key: Option<String>,
}

#[derive(Row, Serialize, Deserialize)]
struct UpdateStateRow {
    pts: i32,
    qts: i32,
    date: i32,
    seq: i32,
}

#[derive(Row, Serialize, Deserialize)]
struct ChannelStateRow {
    peer_id: i64,
    pts: i32,
}

// ── In-memory cache ─────────────────────────────────────────────────

struct Cache {
    home_dc: i32,
    dc_options: HashMap<i32, DcOption>,
    updates: UpdatesState,
}

// ── ClickhouseSession ───────────────────────────────────────────────

pub struct ClickhouseSession {
    ch: clickhouse::Client,
    cache: Mutex<Cache>,
}

impl ClickhouseSession {
    pub async fn open(ch: clickhouse::Client) -> anyhow::Result<Self> {
        let defaults = SessionData::default();

        let home_dc = ch
            .query("SELECT dc_id FROM session_dc_home FINAL WHERE key = 1 LIMIT 1")
            .fetch_one::<DcHomeRow>()
            .await
            .ok()
            .map(|r| r.dc_id)
            .unwrap_or(defaults.home_dc);

        // Load dc_options
        let mut dc_options: HashMap<i32, DcOption> = defaults.dc_options;
        let rows: Vec<DcOptionRow> = ch
            .query("SELECT dc_id, ipv4, ipv6, auth_key FROM session_dc_option FINAL")
            .fetch_all()
            .await
            .unwrap_or_default();
        for row in rows {
            if let Some(opt) = dc_option_from_row(&row) {
                dc_options.insert(opt.id, opt);
            }
        }

        // Load updates state
        let updates = ch
            .query("SELECT pts, qts, date, seq FROM session_update_state FINAL WHERE key = 1 LIMIT 1")
            .fetch_one::<UpdateStateRow>()
            .await
            .ok()
            .map(|r| UpdatesState {
                pts: r.pts,
                qts: r.qts,
                date: r.date,
                seq: r.seq,
                channels: Vec::new(),
            })
            .unwrap_or_default();

        let channels: Vec<ChannelStateRow> = ch
            .query("SELECT peer_id, pts FROM session_channel_state FINAL")
            .fetch_all()
            .await
            .unwrap_or_default();

        let updates = UpdatesState {
            channels: channels
                .into_iter()
                .map(|r| ChannelState {
                    id: r.peer_id,
                    pts: r.pts,
                })
                .collect(),
            ..updates
        };

        Ok(Self {
            ch,
            cache: Mutex::new(Cache {
                home_dc,
                dc_options,
                updates,
            }),
        })
    }
}

// ── Peer encoding / decoding ────────────────────────────────────────

#[repr(u8)]
enum PeerSubtype {
    UserSelf = 1,
    UserBot = 2,
    UserSelfBot = 3,
    Megagroup = 4,
    Broadcast = 8,
    Gigagroup = 12,
    Community = 16,
}

fn encode_subtype(peer: &PeerInfo) -> Option<u8> {
    match peer {
        PeerInfo::User { bot, is_self, .. } => {
            match (bot.unwrap_or_default(), is_self.unwrap_or_default()) {
                (true, true) => Some(PeerSubtype::UserSelfBot as u8),
                (true, false) => Some(PeerSubtype::UserBot as u8),
                (false, true) => Some(PeerSubtype::UserSelf as u8),
                (false, false) => None,
            }
        }
        PeerInfo::Chat { .. } => None,
        PeerInfo::Channel { kind, .. } => kind.map(|k| match k {
            ChannelKind::Megagroup => PeerSubtype::Megagroup as u8,
            ChannelKind::Broadcast => PeerSubtype::Broadcast as u8,
            ChannelKind::Gigagroup => PeerSubtype::Gigagroup as u8,
            ChannelKind::Community => PeerSubtype::Community as u8,
        }),
    }
}

fn peer_to_row(peer: &PeerInfo) -> PeerRow {
    PeerRow {
        peer_id: peer.id().bot_api_dialog_id_unchecked(),
        hash: peer.auth().map(|a| a.hash()),
        subtype: encode_subtype(peer),
        updated_at: now(),
    }
}

fn decode_peer(peer_id: PeerId, row: &PeerRow) -> PeerInfo {
    match peer_id.kind() {
        PeerKind::User => PeerInfo::User {
            id: peer_id.bare_id_unchecked(),
            auth: row.hash.map(PeerAuth::from_hash),
            bot: row.subtype.map(|s| s & PeerSubtype::UserBot as u8 != 0),
            is_self: row.subtype.map(|s| s & PeerSubtype::UserSelf as u8 != 0),
        },
        PeerKind::Chat => PeerInfo::Chat {
            id: peer_id.bare_id_unchecked(),
        },
        PeerKind::Channel => PeerInfo::Channel {
            id: peer_id.bare_id_unchecked(),
            auth: row.hash.map(PeerAuth::from_hash),
            kind: row.subtype.and_then(|s| {
                if (s & PeerSubtype::Gigagroup as u8) == PeerSubtype::Gigagroup as u8 {
                    Some(ChannelKind::Gigagroup)
                } else if s & PeerSubtype::Broadcast as u8 != 0 {
                    Some(ChannelKind::Broadcast)
                } else if s & PeerSubtype::Megagroup as u8 != 0 {
                    Some(ChannelKind::Megagroup)
                } else {
                    None
                }
            }),
        },
    }
}

// ── DcOption ↔ ClickHouse helpers ───────────────────────────────────

fn auth_key_to_hex(key: &[u8; 256]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn auth_key_from_hex(hex: &str) -> Option<[u8; 256]> {
    if hex.len() != 512 {
        return None;
    }
    let mut key = [0u8; 256];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).ok()?;
        key[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(key)
}

fn dc_option_to_row(opt: &DcOption) -> DcOptionRow {
    DcOptionRow {
        dc_id: opt.id,
        ipv4: opt.ipv4.to_string(),
        ipv6: opt.ipv6.to_string(),
        auth_key: opt.auth_key.as_ref().map(auth_key_to_hex),
    }
}

fn dc_option_from_row(row: &DcOptionRow) -> Option<DcOption> {
    Some(DcOption {
        id: row.dc_id,
        ipv4: row.ipv4.parse().ok()?,
        ipv6: row.ipv6.parse().ok()?,
        auth_key: row.auth_key.as_deref().and_then(auth_key_from_hex),
    })
}

// ── The peer table ──────────────────────────────────────────────────

/// The Buffer table in front of `peer_cache` (migration 041). Written and read
/// through, so nothing about a peer is held back in the bot.
///
/// `cache_peer` is called for every peer grammers sees — every sender and chat
/// of every update, and a whole dialog list at once after a sync — and almost
/// always with the row already stored. Nothing here filters the repeats: they
/// go into the Buffer, which is memory, and `ReplacingMergeTree` collapses them
/// on the way down.
const PEER_CACHE: &str = "peer_cache_buffer";

// ── Session trait ───────────────────────────────────────────────────

impl Session for ClickhouseSession {
    // Writes are best-effort (in-memory cache is the source of truth, ClickHouse is
    // write-behind persistence), so the write methods always return `Ok` and only log
    // failures. The one method that genuinely reads from ClickHouse — `peer` — retries
    // transient failures and surfaces a real error if ClickHouse stays unreachable,
    // instead of masking an outage as a missing peer.
    type Error = clickhouse::error::Error;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        Ok(self.cache.lock().unwrap_or_else(PoisonError::into_inner).home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).home_dc = dc_id;
        Box::pin(async move {
            persist(&self.ch, "session_dc_home", DcHomeRow { dc_id }).await;
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        Ok(self.cache.lock().unwrap_or_else(PoisonError::into_inner).dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dc_options
            .insert(dc_option.id, dc_option.clone());

        let row = dc_option_to_row(dc_option);
        Box::pin(async move {
            persist(&self.ch, "session_dc_option", row).await;
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move {
            const MAX_ATTEMPTS: u32 = 5;
            let is_self_query = peer.bot_api_dialog_id().is_none();

            let mut attempt = 0;
            loop {
                attempt += 1;

                let result = if !is_self_query {
                    let dialog_id = peer.bot_api_dialog_id().unwrap();
                    self.ch
                        .query(
                            // argMax over the version rather than FINAL, which
                            // ClickHouse recommends against and which a Buffer
                            // does not apply to its own rows anyway.
                            // The table qualification is load-bearing: the row
                            // is deserialised by column NAME, so each alias has
                            // to be the struct's field name -- but an alias
                            // equal to the column it aggregates shadows it, and
                            // `argMax(hash, updated_at)` then reads the alias
                            // and is rejected as a nested aggregate (Code 184).
                            "SELECT peer_id, \
                                    argMax(peer_cache_buffer.hash, peer_cache_buffer.updated_at) AS hash, \
                                    argMax(peer_cache_buffer.subtype, peer_cache_buffer.updated_at) AS subtype, \
                                    max(peer_cache_buffer.updated_at) AS updated_at \
                             FROM peer_cache_buffer WHERE peer_id = ? \
                             GROUP BY peer_id",
                        )
                        .bind(dialog_id)
                        .fetch_one::<PeerRow>()
                        .await
                } else {
                    self.ch
                        .query(
                            // The WHERE narrows to peers that carried the self
                            // bit in any version -- it is what makes this a
                            // lookup rather than a scan of every peer ever
                            // cached -- and the HAVING asks the same of the
                            // collapsed row, so a peer that has since lost the
                            // bit cannot answer for the account.
                            // The WHERE is qualified for the same reason as the
                            // aggregates -- unqualified, `subtype` there would
                            // resolve to the alias and land an aggregate in a
                            // WHERE. The HAVING is deliberately NOT qualified:
                            // there it is the collapsed value that has to carry
                            // the bit.
                            "SELECT peer_id, \
                                    argMax(peer_cache_buffer.hash, peer_cache_buffer.updated_at) AS hash, \
                                    argMax(peer_cache_buffer.subtype, peer_cache_buffer.updated_at) AS subtype, \
                                    max(peer_cache_buffer.updated_at) AS updated_at \
                             FROM peer_cache_buffer \
                             WHERE peer_cache_buffer.subtype IS NOT NULL \
                               AND bitAnd(peer_cache_buffer.subtype, 1) = 1 \
                             GROUP BY peer_id \
                             HAVING bitAnd(subtype, 1) = 1 \
                             LIMIT 1",
                        )
                        .fetch_one::<PeerRow>()
                        .await
                };

                match result {
                    Ok(row) => {
                        let resolved = if is_self_query {
                            debug!("self user found in clickhouse (peer_id={})", row.peer_id);
                            PeerId::user_unchecked(row.peer_id)
                        } else {
                            debug!("peer {:?} found in clickhouse", peer);
                            peer
                        };
                        return Ok(Some(decode_peer(resolved, &row)));
                    }
                    // Genuine cache miss: the peer simply isn't stored. Return `None`
                    // so grammers resolves it from the network.
                    Err(clickhouse::error::Error::RowNotFound) => {
                        debug!("peer {:?} not in clickhouse", peer);
                        return Ok(None);
                    }
                    // Transient failure (ClickHouse down, network blip): retry a few
                    // times so a brief outage isn't mistaken for a missing peer.
                    Err(e) if attempt < MAX_ATTEMPTS => {
                        warn!(
                            "peer {:?} lookup failed (attempt {}/{}): {} — retrying",
                            peer, attempt, MAX_ATTEMPTS, e
                        );
                        tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
                    }
                    // Out of retries: surface the real error instead of pretending the
                    // peer is missing, so the caller fails loudly with the actual cause.
                    Err(e) => {
                        error!(
                            "peer {:?} lookup failed after {} attempts: {}",
                            peer, MAX_ATTEMPTS, e
                        );
                        return Err(e);
                    }
                }
            }
        })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            let row = peer_to_row(&peer);
            if let Err(e) = insert_rows(&self.ch, PEER_CACHE, std::slice::from_ref(&row)).await {
                error!("insert into {PEER_CACHE}: {e}");
            }
            Ok(())
        })
    }

    /// Bulk variant of [`cache_peer`]: grammers hands us a whole batch after a
    /// dialogs sync or a large update, and they go down as a single insert
    /// rather than a request per peer.
    fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            let rows: Vec<PeerRow> = peers.iter().map(peer_to_row).collect();
            if let Err(e) = insert_rows(&self.ch, PEER_CACHE, &rows).await {
                error!("insert into {PEER_CACHE}: {e}");
            }
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async move { Ok(self.cache.lock().unwrap_or_else(PoisonError::into_inner).updates.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            // Update in-memory cache
            {
                let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
                match &update {
                    UpdateState::All(state) => {
                        cache.updates = state.clone();
                    }
                    UpdateState::Primary { pts, date, seq } => {
                        cache.updates.pts = *pts;
                        cache.updates.date = *date;
                        cache.updates.seq = *seq;
                    }
                    UpdateState::Secondary { qts } => {
                        cache.updates.qts = *qts;
                    }
                    UpdateState::Channel { id, pts } => {
                        if let Some(ch) = cache.updates.channels.iter_mut().find(|c| c.id == *id) {
                            ch.pts = *pts;
                        } else {
                            cache.updates.channels.push(ChannelState {
                                id: *id,
                                pts: *pts,
                            });
                        }
                    }
                }
            }

            // Persist to ClickHouse
            match &update {
                UpdateState::All(state) => {
                    // Write full update_state
                    persist(&self.ch, "session_update_state", UpdateStateRow {
                                pts: state.pts,
                                qts: state.qts,
                                date: state.date,
                                seq: state.seq,
                            }).await;

                    // Replace all channel states: truncate + re-insert
                    if let Err(e) = self.ch
                        .query("TRUNCATE TABLE session_channel_state")
                        .execute()
                        .await
                    {
                        warn!("failed to truncate channel_state: {e}");
                    }
                    // One insert for every channel, right after the truncate,
                    // so the window with no channel state is a single round trip.
                    let rows: Vec<ChannelStateRow> = state
                        .channels
                        .iter()
                        .map(|ch| ChannelStateRow { peer_id: ch.id, pts: ch.pts })
                        .collect();
                    if let Err(e) =
                        insert_rows(&self.ch, "session_channel_state", &rows).await
                    {
                        error!("failed to write session_channel_state: {e}");
                    }
                }
                UpdateState::Primary { pts, date, seq } => {
                    let row = {
                        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
                        UpdateStateRow {
                            pts: *pts,
                            qts: cache.updates.qts,
                            date: *date,
                            seq: *seq,
                        }
                    };
                    persist(&self.ch, "session_update_state", row).await;
                }
                UpdateState::Secondary { qts } => {
                    let row = {
                        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
                        UpdateStateRow {
                            pts: cache.updates.pts,
                            qts: *qts,
                            date: cache.updates.date,
                            seq: cache.updates.seq,
                        }
                    };
                    persist(&self.ch, "session_update_state", row).await;
                }
                UpdateState::Channel { id, pts } => {
                    persist(&self.ch, "session_channel_state", ChannelStateRow {
                                peer_id: *id,
                                pts: *pts,
                            }).await;
                }
            }
            Ok(())
        })
    }
}

/// Write one session row, saying so when it fails: a lost update position
/// means the next start resumes from an older one, and that should be visible.
async fn persist<T>(ch: &clickhouse::Client, table: &str, row: T)
where
    T: serde::Serialize + Send + 'static,
    for<'a> T: clickhouse::Row<Value<'a> = T>,
{
    if let Err(e) = insert_rows(ch, table, std::slice::from_ref(&row)).await {
        error!("failed to write {table}: {e}");
    }
}
