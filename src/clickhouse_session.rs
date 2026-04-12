use std::collections::HashMap;
use std::sync::Mutex;

use clickhouse::Row;
use futures_core::future::BoxFuture;
use grammers_session::types::{
    ChannelKind, ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, PeerKind, UpdateState,
    UpdatesState,
};
use grammers_session::{Session, SessionData};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};

use crate::db::clickhouse;

// ── ClickHouse row types ────────────────────────────────────────────

#[derive(Row, Serialize, Deserialize)]
struct PeerRow {
    peer_id: i64,
    hash: Option<i64>,
    subtype: Option<u8>,
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
    cache: Mutex<Cache>,
}

impl ClickhouseSession {
    pub async fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let defaults = SessionData::default();

        let home_dc = clickhouse()
            .query("SELECT dc_id FROM session_dc_home FINAL WHERE key = 1 LIMIT 1")
            .fetch_one::<DcHomeRow>()
            .await
            .ok()
            .map(|r| r.dc_id)
            .unwrap_or(defaults.home_dc);

        // Load dc_options
        let mut dc_options: HashMap<i32, DcOption> = defaults.dc_options;
        let rows: Vec<DcOptionRow> = clickhouse()
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
        let updates = clickhouse()
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

        let channels: Vec<ChannelStateRow> = clickhouse()
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
        }),
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

// ── Session trait ───────────────────────────────────────────────────

impl Session for ClickhouseSession {
    fn home_dc_id(&self) -> i32 {
        self.cache.lock().unwrap().home_dc
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, ()> {
        self.cache.lock().unwrap().home_dc = dc_id;
        Box::pin(async move {
            if let Ok(mut ins) = clickhouse().insert::<DcHomeRow>("session_dc_home").await {
                if let Err(e) = ins.write(&DcHomeRow { dc_id }).await {
                    error!("failed to write home_dc to clickhouse: {e}");
                } else if let Err(e) = ins.end().await {
                    error!("failed to flush home_dc to clickhouse: {e}");
                }
            }
        })
    }

    fn dc_option(&self, dc_id: i32) -> Option<DcOption> {
        self.cache.lock().unwrap().dc_options.get(&dc_id).cloned()
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, ()> {
        self.cache
            .lock()
            .unwrap()
            .dc_options
            .insert(dc_option.id, dc_option.clone());

        let row = dc_option_to_row(dc_option);
        Box::pin(async move {
            if let Ok(mut ins) = clickhouse().insert::<DcOptionRow>("session_dc_option").await {
                if let Err(e) = ins.write(&row).await {
                    error!("failed to write dc_option to clickhouse: {e}");
                } else if let Err(e) = ins.end().await {
                    error!("failed to flush dc_option to clickhouse: {e}");
                }
            }
        })
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
                        Some(decode_peer(peer, &row))
                    }
                    Err(e) => {
                        debug!("peer {} not in clickhouse: {}", dialog_id, e);
                        None
                    }
                }
            } else {
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
                        Some(decode_peer(resolved, &row))
                    }
                    Err(e) => {
                        debug!("self user not in clickhouse: {}", e);
                        None
                    }
                }
            }
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
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, UpdatesState> {
        Box::pin(async move { self.cache.lock().unwrap().updates.clone() })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Update in-memory cache
            {
                let mut cache = self.cache.lock().unwrap();
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
                    if let Ok(mut ins) = clickhouse()
                        .insert::<UpdateStateRow>("session_update_state")
                        .await
                    {
                        let _ = ins
                            .write(&UpdateStateRow {
                                pts: state.pts,
                                qts: state.qts,
                                date: state.date,
                                seq: state.seq,
                            })
                            .await;
                        let _ = ins.end().await;
                    }

                    // Replace all channel states: truncate + re-insert
                    if let Err(e) = clickhouse()
                        .query("TRUNCATE TABLE session_channel_state")
                        .execute()
                        .await
                    {
                        warn!("failed to truncate channel_state: {e}");
                    }
                    for ch in &state.channels {
                        if let Ok(mut ins) = clickhouse()
                            .insert::<ChannelStateRow>("session_channel_state")
                            .await
                        {
                            let _ = ins
                                .write(&ChannelStateRow {
                                    peer_id: ch.id,
                                    pts: ch.pts,
                                })
                                .await;
                            let _ = ins.end().await;
                        }
                    }
                }
                UpdateState::Primary { pts, date, seq } => {
                    let row = {
                        let cache = self.cache.lock().unwrap();
                        UpdateStateRow {
                            pts: *pts,
                            qts: cache.updates.qts,
                            date: *date,
                            seq: *seq,
                        }
                    };
                    if let Ok(mut ins) = clickhouse()
                        .insert::<UpdateStateRow>("session_update_state")
                        .await
                    {
                        let _ = ins.write(&row).await;
                        let _ = ins.end().await;
                    }
                }
                UpdateState::Secondary { qts } => {
                    let row = {
                        let cache = self.cache.lock().unwrap();
                        UpdateStateRow {
                            pts: cache.updates.pts,
                            qts: *qts,
                            date: cache.updates.date,
                            seq: cache.updates.seq,
                        }
                    };
                    if let Ok(mut ins) = clickhouse()
                        .insert::<UpdateStateRow>("session_update_state")
                        .await
                    {
                        let _ = ins.write(&row).await;
                        let _ = ins.end().await;
                    }
                }
                UpdateState::Channel { id, pts } => {
                    if let Ok(mut ins) = clickhouse()
                        .insert::<ChannelStateRow>("session_channel_state")
                        .await
                    {
                        let _ = ins
                            .write(&ChannelStateRow {
                                peer_id: *id,
                                pts: *pts,
                            })
                            .await;
                        let _ = ins.end().await;
                    }
                }
            }
        })
    }
}
