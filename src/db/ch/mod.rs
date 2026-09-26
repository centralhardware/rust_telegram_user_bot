//! [`Db`] over ClickHouse. Every query the bot runs lives in this module, one
//! file per table; the trait impl below only routes to them.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use clickhouse::{Client, Row};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};

use super::{
    AdminAction, DbResult, DeletedMessage, Event, EventKind, EVENTS, MediaFile, MessageInfo,
    ReplyRow, ReplyTarget, TelegramSession, Db,
};
use crate::state::peer_names::PeerNames;
use crate::state::poll_info::PollInfo;

pub struct ClickhouseDb {
    ch: Client,
    /// When inserts started failing, `None` while the last one went through.
    failing_since: Mutex<Option<Instant>>,
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
            failing_since: Mutex::new(None),
        }
    }

    /// Write rows, and remember whether it worked: see [`Db::writes_failing_for`].
    async fn insert<T>(&self, table: &str, rows: &[T]) -> Result<(), clickhouse::error::Error>
    where
        T: Serialize + Send + 'static,
        for<'a> T: Row<Value<'a> = T>,
    {
        let result = insert_rows(&self.ch, table, rows).await;
        let mut since = self.failing_since.lock().unwrap_or_else(PoisonError::into_inner);
        match &result {
            Ok(()) => *since = None,
            Err(_) => {
                since.get_or_insert_with(Instant::now);
            }
        }
        result
    }

    /// The raw client, for the session store, which is infrastructure rather
    /// than something a handler asks for.
    pub fn client(&self) -> &Client {
        &self.ch
    }
}

/// Write rows to a table. Nothing is queued here: `async_insert` on the client
/// means the server holds the rows and decides when they become a part.
pub async fn insert_rows<T>(ch: &Client, table: &str, rows: &[T]) -> Result<(), clickhouse::error::Error>
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
    fn writes_failing_for(&self) -> Option<Duration> {
        self.failing_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map(|since| since.elapsed())
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_insert_starts_the_failure_clock() {
        // Nothing listens on port 1, so every insert fails at once.
        let db = ClickhouseDb {
            ch: Client::default().with_url("http://127.0.0.1:1"),
            failing_since: Mutex::new(None),
        };
        assert_eq!(db.writes_failing_for(), None);

        assert!(db.write_backfill(&[Event::default()]).await.is_err());
        let first = db.writes_failing_for().expect("failing after a failed insert");

        // A second failure does not restart the clock.
        assert!(db.write_backfill(&[Event::default()]).await.is_err());
        assert!(db.writes_failing_for().unwrap() >= first);
    }
}
