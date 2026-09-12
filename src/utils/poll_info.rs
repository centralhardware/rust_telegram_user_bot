//! What a poll is, looked up by its id.
//!
//! A results update names the message — and the poll itself — only sometimes.
//! When it does not, all it carries is `poll_id`, which is why the send row
//! stores that too: the poll's chat, question and option wording are read back
//! from it here, so a vote is logged and shown as the poll it belongs to rather
//! than as a bare id and a row of indexes.

use clickhouse::Row;
use log::{debug, error};
use serde::Deserialize;

use crate::db::{EVENTS_BUF, EDIT, SEND};

#[derive(Row, Deserialize, Clone, Default, Debug)]
pub struct PollInfo {
    pub chat_id: i64,
    pub chat_title: String,
    pub message_id: i64,
    pub question: String,
    pub options: Vec<String>,
}

/// The poll a results update belongs to, or `None` when its message was never
/// seen — a poll sent before the logger was running, say.
///
/// The buffer is checked first: a poll sent this minute is not in ClickHouse
/// yet, and the first vote on a fresh poll usually lands in that window.
/// Unmemoised, like the peer names: the send row is the only place a poll's
/// wording lives, and an edit that rewrites it is picked up on the next vote.
pub async fn load(poll_id: i64) -> Option<PollInfo> {
    if poll_id == 0 {
        return None;
    }

    if let Some(info) = EVENTS_BUF
        .find_last(|e| {
            ((e.event == SEND || e.event == EDIT)
                && e.poll_id == poll_id
                && !e.poll_question.is_empty())
            .then(|| PollInfo {
                chat_id: e.chat_id,
                chat_title: e.chat_title.clone(),
                message_id: e.message_id,
                question: e.poll_question.clone(),
                options: e.poll_options.clone(),
            })
        })
        .await
    {
        return Some(info);
    }

    match crate::db::clickhouse()
        .query(
            "SELECT chat_id, chat_title, message_id, poll_question, poll_options \
             FROM events_log \
             WHERE poll_id = ? AND event IN (?, ?) AND poll_question != '' \
             ORDER BY date_time DESC LIMIT 1",
        )
        .bind(poll_id)
        .bind(SEND)
        .bind(EDIT)
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
