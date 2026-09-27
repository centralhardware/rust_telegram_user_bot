//! [`Db`] over ClickHouse. Every query the bot runs lives in this module, one
//! file per table; the trait impl below only routes to them.

use std::collections::HashSet;

use async_trait::async_trait;
use clickhouse::{Client, Row};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};

use super::{
    AdminAction, Db, DbResult, DeletedMessage, EVENTS, Event, EventKind, MediaFile, MessageInfo,
    ReplyRow, ReplyTarget, TelegramSession,
};
use crate::state::peer_names::PeerNames;
use crate::state::poll_info::PollInfo;

pub struct ClickhouseDb {
    ch: Client,
    /// The `account_id` this account's private-chat and basic-group rows carry
    /// (migration 049): 0 for the first account, the user id for a later one.
    account: u64,
    /// For a later account, the chats known to be channels, and where to ask
    /// about the rest. See [`ClickhouseDb::scope`].
    channels: Option<Channels>,
}

struct Channels {
    peer_cache: String,
    known: std::sync::Mutex<HashSet<i64>>,
}

impl ClickhouseDb {
    /// The client, from the `CLICKHOUSE_*` settings. Panics on a missing one,
    /// so a misconfigured bot stops at startup rather than at the first write.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} not set"));
        let ch = Client::default()
            .with_url(var("CLICKHOUSE_URL"))
            .with_user(var("CLICKHOUSE_USER"))
            .with_password(var("CLICKHOUSE_PASSWORD"))
            .with_database(var("CLICKHOUSE_DATABASE"))
            // Many small writes — one per event, one per update position — so the
            // server batches them into parts. Not waiting for the flush: an insert
            // returns once the server has the rows, and only a connection or
            // parsing failure comes back as an error.
            .with_setting("async_insert", "1")
            .with_setting("wait_for_async_insert", "0");
        ClickhouseDb {
            ch,
            account: 0,
            channels: None,
        }
    }

    /// The same database, for a later account: `account` is its user id,
    /// `name` what its session tables are suffixed with.
    /// `""` is the first account, whose rows are all 0.
    pub fn for_account(&self, account: u64, name: &str) -> Self {
        if name.is_empty() {
            return ClickhouseDb {
                ch: self.ch.clone(),
                account: 0,
                channels: None,
            };
        }
        ClickhouseDb {
            ch: self.ch.clone(),
            account,
            channels: Some(Channels {
                peer_cache: format!("peer_cache_buffer_{name}"),
                known: Default::default(),
            }),
        }
    }

    /// The `account_id` a row of this chat carries: channels are shared by
    /// every account (0), other chats number their messages per account.
    ///
    /// `chat_id` in `events_log` is the bare id, which does not say whether it
    /// is a channel. The first account never needs to know -- all its rows are
    /// 0. A later one asks its own peer cache, where grammers has put every
    /// channel the account has had an update from, under its `-100…` id,
    /// before any handler runs. Only a yes is remembered: a chat not cached
    /// yet may be by the next lookup.
    async fn scope(&self, chat_id: i64) -> u64 {
        let Some(channels) = &self.channels else {
            return 0;
        };
        let dialog_id = -1_000_000_000_000 - chat_id;
        if chat_id > 0
            && channels
                .known
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&chat_id)
        {
            return 0;
        }
        let cached = chat_id > 0
            && self
                .ch
                .query(&format!(
                    "SELECT count() FROM {} WHERE peer_id = ?",
                    channels.peer_cache
                ))
                .bind(dialog_id)
                .fetch_one::<u64>()
                .await
                .is_ok_and(|n| n > 0);
        if cached {
            channels
                .known
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(chat_id);
            0
        } else {
            self.account
        }
    }

    /// The raw client, for the session store, which is infrastructure rather
    /// than something a handler asks for.
    pub fn client(&self) -> &Client {
        &self.ch
    }
}

/// Write rows to a table. Nothing is queued here: `async_insert` on the client
/// means the server holds the rows and decides when they become a part.
pub async fn insert_rows<T>(
    ch: &Client,
    table: &str,
    rows: &[T],
) -> Result<(), clickhouse::error::Error>
where
    T: Serialize + Send + 'static,
    for<'a> T: Row<Value<'a> = T>,
{
    let mut insert = ch.insert::<T>(table).await?;
    for row in rows {
        insert.write(row).await?;
    }
    insert.end().await
}

/// Whether a Bot API dialog id -- not an `events_log` chat id, which is bare --
/// is a channel or supergroup (`-100…`).
pub fn is_channel(chat_id: i64) -> bool {
    chat_id <= -1_000_000_000_000
}

const INSERT_ATTEMPTS: u32 = 3;

/// The body of a message as the log has it, read back for an edit.
#[derive(Row, Deserialize, Default)]
struct BodyRow {
    message: String,
    entities: Vec<crate::telegram::entities::Entity>,
    keyboard: Vec<crate::telegram::entities::Button>,
}

#[derive(Row, Deserialize)]
struct LastChatRow {
    title: String,
    usernames: Vec<String>,
}

mod admin_actions;
mod events;
mod media_files;
mod peer_names;
mod user_sessions;

#[async_trait]
impl Db for ClickhouseDb {
    async fn log_events(&self, events: &[Event]) {
        ClickhouseDb::log_events(self, events).await
    }

    async fn write_backfill(&self, events: &[Event]) -> DbResult<()> {
        ClickhouseDb::write_backfill(self, events).await
    }

    async fn find_message(&self, chat_id: i64, message_id: i64) -> MessageInfo {
        ClickhouseDb::find_message(self, chat_id, message_id).await
    }

    async fn find_deleted(&self, channel: Option<i64>, message_ids: &[i64]) -> Vec<DeletedMessage> {
        ClickhouseDb::find_deleted(self, channel, message_ids).await
    }

    async fn find_target(&self, chat_id: i64, message_id: i64) -> ReplyTarget {
        ClickhouseDb::find_target(self, chat_id, message_id).await
    }

    async fn find_reply_row(&self, chat_id: i64, message_id: i64) -> Option<ReplyRow> {
        ClickhouseDb::find_reply_row(self, chat_id, message_id).await
    }

    async fn message_exists(&self, chat_id: i64, message_id: i64) -> bool {
        ClickhouseDb::message_exists(self, chat_id, message_id).await
    }

    async fn known_ids(&self, chat_id: i64, ids: &[i64]) -> DbResult<HashSet<i64>> {
        ClickhouseDb::known_ids(self, chat_id, ids).await
    }

    async fn oldest_logged_id(&self, chat_id: i64) -> DbResult<i64> {
        ClickhouseDb::oldest_logged_id(self, chat_id).await
    }

    async fn logged_chat_ids(&self) -> DbResult<HashSet<i64>> {
        ClickhouseDb::logged_chat_ids(self).await
    }

    async fn last_chat_name(&self, chat_id: i64) -> Option<(String, Vec<String>)> {
        ClickhouseDb::last_chat_name(self, chat_id).await
    }

    async fn find_poll(&self, poll_id: i64) -> Option<PollInfo> {
        ClickhouseDb::find_poll(self, poll_id).await
    }

    async fn find_media_file(&self, kind: &str, tg_id: i64) -> Option<MediaFile> {
        ClickhouseDb::find_media_file(self, kind, tg_id).await
    }

    async fn remember_media_file(&self, file: MediaFile) {
        ClickhouseDb::remember_media_file(self, file).await
    }

    async fn load_peer_names(&self, peer_id: i64) -> Option<PeerNames> {
        ClickhouseDb::load_peer_names(self, peer_id).await
    }

    async fn write_peer_names(&self, names: &PeerNames) {
        ClickhouseDb::write_peer_names(self, names).await
    }

    async fn last_admin_event_id(&self, chat_id: u64) -> u64 {
        ClickhouseDb::last_admin_event_id(self, chat_id).await
    }

    async fn write_admin_actions(&self, actions: &[AdminAction]) -> DbResult<()> {
        ClickhouseDb::write_admin_actions(self, actions).await
    }

    async fn write_user_sessions(&self, sessions: &[TelegramSession]) -> DbResult<()> {
        ClickhouseDb::write_user_sessions(self, sessions).await
    }
}
