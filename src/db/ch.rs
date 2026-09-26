//! [`Db`] over ClickHouse: every query the bot runs, in one place.

use std::collections::HashSet;

use async_trait::async_trait;
use clickhouse::{Client, Row};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};

use super::{
    AdminAction, DbResult, DeletedMessage, Event, EventKind, EVENTS, MediaFile, MessageInfo,
    ReplyRow, ReplyTarget, TelegramSession, Db,
};
use crate::utils::peer_names::PeerNames;
use crate::utils::poll_info::PollInfo;

pub struct ClickhouseDb {
    ch: Client,
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
        ClickhouseDb { ch }
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
    entities: Vec<crate::utils::entities::Entity>,
    keyboard: Vec<crate::utils::entities::Button>,
}

#[derive(Row, Deserialize)]
struct LastChatRow {
    title: String,
    usernames: Vec<String>,
}

#[async_trait]
impl Db for ClickhouseDb {
    async fn log_events(&self, events: &[Event]) {
        let mut delay = std::time::Duration::from_millis(500);
        for attempt in 1..=INSERT_ATTEMPTS {
            match insert_rows(&self.ch, EVENTS, events).await {
                Ok(()) => return,
                Err(e) if attempt == INSERT_ATTEMPTS => {
                    error!("insert into {EVENTS}: {e}");
                }
                Err(e) => {
                    warn!("insert into {EVENTS} (attempt {attempt}): {e}");
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
    }

    async fn write_backfill(&self, events: &[Event]) -> DbResult<()> {
        Ok(insert_rows(&self.ch, "events_log", events).await?)
    }

    async fn find_message(&self, chat_id: i64, message_id: i64) -> MessageInfo {
        let body = self
            .ch
            .query(
                "SELECT message, entities, keyboard FROM events_log_buffer \
                 WHERE chat_id = ? AND message_id = ? AND event IN (?, ?) \
                 ORDER BY event = ? DESC, date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(message_id)
            .bind(EventKind::Send)
            .bind(EventKind::Edit)
            .bind(EventKind::Edit)
            .fetch_one::<BodyRow>()
            .await;
        let logged = body.is_ok();
        let body = body.unwrap_or_default();

        let chat_title = self
            .ch
            .query(
                "SELECT chat_title FROM events_log_buffer \
                 WHERE chat_id = ? AND event = ? AND chat_title != '' \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(EventKind::Send)
            .fetch_one::<String>()
            .await
            .unwrap_or_default();

        MessageInfo {
            logged,
            message: body.message,
            entities: body.entities,
            keyboard: body.keyboard,
            chat_title,
        }
    }

    async fn find_deleted(&self, channel: Option<i64>, message_ids: &[i64]) -> Vec<DeletedMessage> {
        let chats = match channel {
            Some(_) => "chat_id = ?",
            None => "chat_id IN (SELECT if(peer_id > 0, peer_id, -peer_id) FROM peer_names_buffer \
                     WHERE peer_id > -1000000000000)",
        };
        let sql = format!(
            "WITH m AS ( \
                 SELECT chat_id, message_id, \
                        argMax(message, (event = ?, date_time)) AS message, \
                        argMaxIf(user_id, date_time, event = ?) AS user_id \
                 FROM events_log_buffer \
                 WHERE {chats} AND has(?, message_id) AND event IN (?, ?) \
                 GROUP BY chat_id, message_id) \
             SELECT m.chat_id AS chat_id, m.message_id AS message_id, m.message AS message, \
                    n.first_name AS first_name, t.title AS chat_title \
             FROM m \
             LEFT JOIN ( \
                 SELECT peer_id, argMax(first_name, updated_at) AS first_name \
                 FROM peer_names_buffer \
                 WHERE peer_id IN (SELECT toInt64(user_id) FROM m WHERE user_id != 0) \
                 GROUP BY peer_id) AS n ON n.peer_id = toInt64(m.user_id) \
             LEFT JOIN ( \
                 SELECT chat_id, argMax(chat_title, date_time) AS title \
                 FROM events_log_buffer \
                 WHERE chat_id IN (SELECT chat_id FROM m) AND event = ? AND chat_title != '' \
                 GROUP BY chat_id) AS t ON t.chat_id = m.chat_id"
        );
        let mut query = self.ch.query(&sql).bind(EventKind::Edit).bind(EventKind::Send);
        if let Some(chat_id) = channel {
            query = query.bind(chat_id);
        }
        query
            .bind(message_ids)
            .bind(EventKind::Send)
            .bind(EventKind::Edit)
            .bind(EventKind::Send)
            .fetch_all::<DeletedMessage>()
            .await
            .unwrap_or_else(|e| {
                warn!("looking up deleted messages {message_ids:?}: {e}");
                Vec::new()
            })
    }

    async fn find_target(&self, chat_id: i64, message_id: i64) -> ReplyTarget {
        self.ch
            .query(
                "SELECT user_id, user_id = 0 AND fwd_from_chat_id != 0 AND fwd_from_msg_id != 0 \
                 FROM events_log_buffer \
                 WHERE chat_id = ? AND message_id = ? AND event = ? \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(message_id)
            .bind(EventKind::Send)
            .fetch_one::<(u64, bool)>()
            .await
            .map(|(user_id, post_copy)| ReplyTarget { user_id, post_copy })
            .unwrap_or_default()
    }

    async fn find_reply_row(&self, chat_id: i64, message_id: i64) -> Option<ReplyRow> {
        self.ch
            .query(
                "SELECT message, user_id, chat_title, fwd_from_chat_id, fwd_from_msg_id \
                 FROM events_log_buffer \
                 WHERE chat_id = ? AND message_id = ? AND event = ? \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(message_id)
            .bind(EventKind::Send)
            .fetch_one::<ReplyRow>()
            .await
            .ok()
    }

    async fn message_exists(&self, chat_id: i64, message_id: i64) -> bool {
        self.ch
            .query(
                "SELECT count() FROM events_log_buffer \
                 WHERE chat_id = ? AND message_id = ? AND event IN (?, ?) AND NOT ephemeral",
            )
            .bind(chat_id)
            .bind(message_id)
            .bind(EventKind::Send)
            .bind(EventKind::Service)
            .fetch_one::<u64>()
            .await
            .is_ok_and(|count| count > 0)
    }

    async fn known_ids(&self, chat_id: i64, ids: &[i64]) -> DbResult<HashSet<i64>> {
        Ok(self
            .ch
            .query(&format!(
                "SELECT message_id FROM {EVENTS} \
                 WHERE chat_id = ? AND event IN (?, ?) AND NOT ephemeral \
                 AND has(?, message_id)"
            ))
            .bind(chat_id)
            .bind(EventKind::Send)
            .bind(EventKind::Service)
            .bind(ids)
            .fetch_all::<i64>()
            .await?
            .into_iter()
            .collect())
    }

    async fn oldest_logged_id(&self, chat_id: i64) -> DbResult<i64> {
        Ok(self
            .ch
            .query(&format!(
                "SELECT min(message_id) FROM {EVENTS} WHERE chat_id = ? AND event = ?"
            ))
            .bind(chat_id)
            .bind(EventKind::Send)
            .fetch_one::<i64>()
            .await?)
    }

    async fn logged_chat_ids(&self) -> DbResult<HashSet<i64>> {
        Ok(self
            .ch
            .query(&format!("SELECT DISTINCT chat_id FROM {EVENTS} WHERE NOT ephemeral"))
            .fetch_all::<i64>()
            .await?
            .into_iter()
            .collect())
    }

    async fn last_chat_name(&self, chat_id: i64) -> Option<(String, Vec<String>)> {
        match self
            .ch
            .query(
                "SELECT argMax(chat_title, date_time) AS title, \
                        argMax(chat_usernames, date_time) AS usernames \
                 FROM events_log_buffer \
                 WHERE chat_id = ? AND event = ? AND chat_title != ''",
            )
            .bind(chat_id)
            .bind(EventKind::Send)
            .fetch_one::<LastChatRow>()
            .await
        {
            // With no row to aggregate the title comes back empty: no name.
            Ok(row) if !row.title.is_empty() => Some((row.title, row.usernames)),
            _ => None,
        }
    }

    async fn find_poll(&self, poll_id: i64) -> Option<PollInfo> {
        match self
            .ch
            .query(
                "SELECT chat_id, chat_title, message_id, poll_question, poll_options \
                 FROM events_log_buffer \
                 WHERE poll_id = ? AND event IN (?, ?) AND poll_question != '' \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(poll_id)
            .bind(EventKind::Send)
            .bind(EventKind::Edit)
            .fetch_one::<PollInfo>()
            .await
        {
            Ok(row) => Some(row),
            Err(clickhouse::error::Error::RowNotFound) => {
                debug!("poll {poll_id} has no stored message");
                None
            }
            Err(e) => {
                error!("looking up poll {poll_id}: {e}");
                None
            }
        }
    }

    async fn find_media_file(&self, kind: &str, tg_id: i64) -> Option<MediaFile> {
        self.ch
            .query(
                "SELECT ?fields FROM media_files FINAL \
                 WHERE kind = ? AND tg_id = ? LIMIT 1",
            )
            .bind(kind)
            .bind(tg_id)
            .fetch_optional::<MediaFile>()
            .await
            .unwrap_or_else(|e| {
                warn!("media_files lookup for {kind} {tg_id}: {e}");
                None
            })
    }

    async fn remember_media_file(&self, file: MediaFile) {
        if let Err(e) = insert_rows(&self.ch, "media_files", std::slice::from_ref(&file)).await {
            warn!("media_files insert for {} {}: {e}", file.kind, file.tg_id);
        }
    }

    async fn load_peer_names(&self, peer_id: i64) -> Option<PeerNames> {
        match self
            .ch
            .query(
                "SELECT peer_id, \
                        argMax(peer_names_buffer.title, peer_names_buffer.updated_at) AS title, \
                        argMax(peer_names_buffer.first_name, peer_names_buffer.updated_at) AS first_name, \
                        argMax(peer_names_buffer.last_name, peer_names_buffer.updated_at) AS last_name, \
                        argMax(peer_names_buffer.usernames, peer_names_buffer.updated_at) AS usernames, \
                        argMax(peer_names_buffer.community_id, peer_names_buffer.updated_at) AS community_id, \
                        max(peer_names_buffer.updated_at) AS updated_at \
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

    async fn write_peer_names(&self, names: &PeerNames) {
        // The Buffer table in front of `peer_names` (migration 041).
        if let Err(e) =
            insert_rows(&self.ch, "peer_names_buffer", std::slice::from_ref(names)).await
        {
            error!("insert into peer_names_buffer: {e}");
        }
    }

    async fn last_admin_event_id(&self, chat_id: u64) -> u64 {
        self.ch
            .query("SELECT max(event_id) FROM admin_actions2 WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one()
            .await
            .unwrap_or(0)
    }

    async fn write_admin_actions(&self, actions: &[AdminAction]) -> DbResult<()> {
        Ok(insert_rows(&self.ch, "admin_actions2", actions).await?)
    }

    async fn write_user_sessions(&self, sessions: &[TelegramSession]) -> DbResult<()> {
        Ok(insert_rows(&self.ch, "user_sessions", sessions).await?)
    }
}
