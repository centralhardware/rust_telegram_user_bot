//! `events_log`, read and written through its Buffer (`events_log_buffer`),
//! except backfill batches, which go straight to the table.

use super::*;

impl ClickhouseDb {
    pub(super) async fn log_events(&self, events: &[Event]) {
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

    pub(super) async fn write_backfill(&self, events: &[Event]) -> DbResult<()> {
        Ok(insert_rows(&self.ch, "events_log", events).await?)
    }

    pub(super) async fn find_message(&self, chat_id: i64, message_id: i64) -> MessageInfo {
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

    pub(super) async fn find_deleted(
        &self,
        channel: Option<i64>,
        message_ids: &[i64],
    ) -> Vec<DeletedMessage> {
        let chats = match channel {
            Some(_) => "chat_id = ?",
            None => {
                "chat_id IN (SELECT if(peer_id > 0, peer_id, -peer_id) FROM peer_names_buffer \
                     WHERE peer_id > -1000000000000)"
            }
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
        let mut query = self
            .ch
            .query(&sql)
            .bind(EventKind::Edit)
            .bind(EventKind::Send);
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

    pub(super) async fn find_target(&self, chat_id: i64, message_id: i64) -> ReplyTarget {
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

    pub(super) async fn find_reply_row(&self, chat_id: i64, message_id: i64) -> Option<ReplyRow> {
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

    pub(super) async fn message_exists(&self, chat_id: i64, message_id: i64) -> bool {
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

    pub(super) async fn known_ids(&self, chat_id: i64, ids: &[i64]) -> DbResult<HashSet<i64>> {
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

    pub(super) async fn oldest_logged_id(&self, chat_id: i64) -> DbResult<i64> {
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

    pub(super) async fn logged_chat_ids(&self) -> DbResult<HashSet<i64>> {
        Ok(self
            .ch
            .query(&format!(
                "SELECT DISTINCT chat_id FROM {EVENTS} WHERE NOT ephemeral"
            ))
            .fetch_all::<i64>()
            .await?
            .into_iter()
            .collect())
    }

    pub(super) async fn last_chat_name(&self, chat_id: i64) -> Option<(String, Vec<String>)> {
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

    pub(super) async fn find_poll(&self, poll_id: i64) -> Option<PollInfo> {
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
}
