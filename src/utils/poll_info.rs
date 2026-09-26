//! What a poll is, looked up by its id.
//!
//! A results update names the message — and the poll itself — only sometimes.
//! When it does not, all it carries is `poll_id`, which is why the send row
//! stores that too: the poll's chat, question and option wording are read back
//! from it here, so a vote is logged and shown as the poll it belongs to rather
//! than as a bare id and a row of indexes.

use clickhouse::Row;
use serde::Deserialize;

use crate::app::App;

#[derive(Row, Deserialize, Clone, Default, Debug)]
pub struct PollInfo {
    pub chat_id: i64,
    pub chat_title: String,
    pub message_id: i64,
    #[serde(rename = "poll_question")]
    pub question: String,
    #[serde(rename = "poll_options")]
    pub options: Vec<String>,
}

/// The poll a results update belongs to, or `None` when its message was never
/// seen — a poll sent before the logger was running, say.
///
/// Unmemoised, like the peer names: the send row is the only place a poll's
/// wording lives, and an edit that rewrites it is picked up on the next vote.
pub async fn load(app: &App, poll_id: i64) -> Option<PollInfo> {
    if poll_id == 0 {
        return None;
    }
    app.db.find_poll(poll_id).await
}
