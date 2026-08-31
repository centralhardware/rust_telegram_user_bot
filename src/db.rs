use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tokio::sync::Mutex;

static CLICKHOUSE: LazyLock<Client> = LazyLock::new(|| {
    Client::default()
        .with_url(std::env::var("CLICKHOUSE_URL").expect("CLICKHOUSE_URL not set"))
        .with_user(std::env::var("CLICKHOUSE_USER").expect("CLICKHOUSE_USER not set"))
        .with_password(std::env::var("CLICKHOUSE_PASSWORD").expect("CLICKHOUSE_PASSWORD not set"))
        .with_database(std::env::var("CLICKHOUSE_DATABASE").expect("CLICKHOUSE_DATABASE not set"))
});

// peer_cache is written one row per newly-seen peer, so synchronous inserts make a part per
// peer. Async inserts let the server batch them, and `wait_for_async_insert=0` keeps the update
// loop from blocking on the flush — a peer that misses the cache is just re-resolved.
static CLICKHOUSE_ASYNC_INSERT: LazyLock<Client> = LazyLock::new(|| {
    CLICKHOUSE
        .clone()
        .with_setting("async_insert", "1")
        .with_setting("wait_for_async_insert", "0")
});

pub fn clickhouse() -> &'static Client {
    &CLICKHOUSE
}

pub fn clickhouse_async_insert() -> &'static Client {
    &CLICKHOUSE_ASYNC_INSERT
}

pub struct WriteBuffer<T: Send + 'static> {
    table: &'static str,
    buffer: Mutex<Vec<T>>,
}

impl<T> WriteBuffer<T>
where
    T: Serialize + Send + 'static,
    for<'a> T: Row<Value<'a> = T>,
{
    pub const fn new(table: &'static str) -> Self {
        Self {
            table,
            buffer: Mutex::const_new(Vec::new()),
        }
    }

    pub async fn push(&self, row: T) {
        self.buffer.lock().await.push(row);
    }

    pub async fn find_last<F, R>(&self, f: F) -> Option<R>
    where
        F: Fn(&T) -> Option<R>,
    {
        self.buffer.lock().await.iter().rev().find_map(f)
    }

    pub async fn flush(&self) -> usize {
        let rows: Vec<T> = {
            let mut buf = self.buffer.lock().await;
            if buf.is_empty() {
                return 0;
            }
            std::mem::take(&mut *buf)
        };
        let count = rows.len();
        match clickhouse().insert::<T>(self.table).await {
            Ok(mut insert) => {
                for row in rows {
                    if let Err(e) = insert.write(&row).await {
                        log::error!("buffer write to {}: {e}", self.table);
                        return 0;
                    }
                }
                if let Err(e) = insert.end().await {
                    log::error!("buffer flush to {}: {e}", self.table);
                    0
                } else {
                    count
                }
            }
            Err(e) => {
                log::error!("buffer insert to {}: {e}", self.table);
                0
            }
        }
    }
}

/// Every message event the account sees — a message sent, edited or deleted — is
/// one row in `events_log`. Which columns are filled depends on `event`; the rest
/// stay at their zero value.
pub static EVENTS_BUF: WriteBuffer<Event> = WriteBuffer::new("events_log");

pub const SEND: &str = "send";
pub const EDIT: &str = "edit";
pub const DELETE: &str = "delete";
pub const REACTION: &str = "reaction";

pub struct MessageInfo {
    pub message: String,
    pub chat_title: String,
    pub user_id: u64,
    pub first_name: String,
    pub topic_id: i32,
    pub topic_name: String,
}

/// What the send row of a message says about who posted it and where. Read back
/// for a deletion, which Telegram reports as a bare id.
#[derive(Row, Deserialize, Default)]
struct SendRow {
    user_id: u64,
    topic_id: i32,
    topic_name: String,
}

/// Find message info by chat_id + message_id: the text as it stands now — the
/// last edit if there was one, the sent text otherwise.
/// Priority: the unflushed buffer → ClickHouse.
pub async fn find_message(chat_id: i64, message_id: i64) -> MessageInfo {
    let sent = EVENTS_BUF
        .find_last(|e| {
            (e.event == SEND && e.chat_id == chat_id && e.message_id == message_id).then(|| {
                (
                    e.message.clone(),
                    e.chat_title.clone(),
                    e.first_name.clone(),
                    SendRow {
                        user_id: e.user_id,
                        topic_id: e.topic_id,
                        topic_name: e.topic_name.clone(),
                    },
                )
            })
        })
        .await;

    let edited = EVENTS_BUF
        .find_last(|e| {
            (e.event == EDIT && e.chat_id == chat_id && e.message_id == message_id)
                .then(|| e.message.clone())
        })
        .await;

    let message = if let Some(msg) = edited {
        msg
    } else if let Some((msg, _, _, _)) = sent.as_ref() {
        msg.clone()
    } else {
        clickhouse()
            .query(
                "SELECT message FROM events_log \
                 WHERE chat_id = ? AND message_id = ? AND event IN (?, ?) \
                 ORDER BY event = ? DESC, date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(message_id)
            .bind(SEND)
            .bind(EDIT)
            .bind(EDIT)
            .fetch_one::<String>()
            .await
            .unwrap_or_default()
    };

    let (chat_title, first_name, send) = if let Some((_, title, name, send)) = sent {
        (title, name, send)
    } else {
        let title = clickhouse()
            .query(
                "SELECT chat_title FROM events_log \
                 WHERE chat_id = ? AND event = ? AND chat_title != '' \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(SEND)
            .fetch_one::<String>()
            .await
            .unwrap_or_default();

        // The row says who sent it and where; the name itself comes from
        // `peer_names`, which is kept current for every peer that passes through —
        // so a sender renamed since the message was logged is named as they are now.
        let send = clickhouse()
            .query(
                "SELECT user_id, topic_id, topic_name FROM events_log \
                 WHERE chat_id = ? AND message_id = ? AND event = ? \
                 ORDER BY date_time DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(message_id)
            .bind(SEND)
            .fetch_one::<SendRow>()
            .await
            .unwrap_or_default();

        let name = match send.user_id {
            0 => String::new(),
            id => crate::utils::peer_names::load(id as i64)
                .await
                .map(|n| n.first_name)
                .unwrap_or_default(),
        };

        (title, name, send)
    };

    MessageInfo {
        message,
        chat_title,
        user_id: send.user_id,
        first_name,
        topic_id: send.topic_id,
        topic_name: send.topic_name,
    }
}

/// Who sent a message, for the `reply_to_user_id` of the message answering it.
/// The unflushed buffer first, then ClickHouse; 0 when the message is older than
/// the log or was never seen.
pub async fn find_sender(chat_id: i64, message_id: i64) -> u64 {
    if let Some(user_id) = EVENTS_BUF
        .find_last(|e| {
            (e.event == SEND && e.chat_id == chat_id && e.message_id == message_id)
                .then_some(e.user_id)
        })
        .await
    {
        return user_id;
    }

    clickhouse()
        .query(
            "SELECT user_id FROM events_log \
             WHERE chat_id = ? AND message_id = ? AND event = ? \
             ORDER BY date_time DESC LIMIT 1",
        )
        .bind(chat_id)
        .bind(message_id)
        .bind(SEND)
        .fetch_one::<u64>()
        .await
        .unwrap_or_default()
}

/// One `events_log` row. Built through `Event::send()` / `edit()` / `delete()`,
/// which name the event and leave every column the event does not use empty.
///
/// A message is one row whatever it carries: the text representation in
/// `message`, the message object itself in `raw`, and — when it carries media —
/// what that media is in the `media_*` columns. A file archived to S3 is the
/// same row written again with `sha256` / `s3_*` filled and a newer `version`,
/// which the ReplacingMergeTree collapses onto the original.
#[derive(Row, Serialize, Default, Clone)]
pub struct Event {
    pub date_time: u32,
    pub event: String,
    pub chat_id: i64,
    pub chat_title: String,
    pub message_id: i64,
    pub message: String,
    pub user_id: u64,
    pub username: Vec<String>,
    pub first_name: String,
    pub second_name: String,
    /// The sender's rank badge in the chat — Telegram's `from_rank`.
    pub community_tag: String,
    /// The community the chat belongs to, 0 when it belongs to none.
    pub community_id: i64,
    pub chat_usernames: Vec<String>,
    /// The message this one replies to, and who sent that message.
    pub reply_to: u64,
    pub reply_to_user_id: u64,
    /// The forum topic the message was posted in, 0 outside a forum.
    pub topic_id: i32,
    pub topic_name: String,
    /// Where a forward came from. `fwd_from_name` is all Telegram gives for a
    /// sender who hides their account behind their name.
    pub fwd_from_user_id: u64,
    pub fwd_from_chat_id: i64,
    pub fwd_from_msg_id: i64,
    pub fwd_from_name: String,
    pub fwd_date: u32,
    /// The service action the message announces, named rather than only spelled
    /// out in `message`. Empty for an ordinary message.
    pub action: String,
    /// The album the message belongs to: one caption, one id, one row per file.
    pub grouped_id: u64,
    /// A 'reaction' row: the counts as they stand after the change.
    pub reactions: Vec<(String, u32)>,
    /// A message only one member of the group can see, whose id belongs to the
    /// ephemeral sequence rather than the chat's.
    pub ephemeral: bool,
    pub receiver_id: u64,
    pub reply_to_ephemeral: bool,
    pub welcome: bool,
    /// This account is the sender.
    pub out: bool,
    /// The message object as Telegram sent it.
    pub raw: String,
    /// The inline bot it was sent through, and the signature a channel post
    /// carries instead of a sender.
    pub via_bot_id: u64,
    /// The peer a guest-chat message actually came from, when `user_id` is only
    /// the relay it arrived through.
    pub guest_from_id: i64,
    pub post_author: String,
    /// Telegram's own flags. `ttl_period` is the self-destruct timer in seconds.
    pub pinned: bool,
    pub silent: bool,
    pub noforwards: bool,
    pub ttl_period: u32,
    /// Edits: the unified diff against the text this edit replaced. That text is
    /// the `message` of the send — or of the previous edit — of the same message,
    /// so it is not stored again here.
    pub diff: String,
    /// What the message carries besides text, and — once the archiver has run —
    /// where the file itself was stored.
    pub media_type: String,
    pub file_name: String,
    pub mime_type: String,
    pub size: u64,
    pub duration: u32,
    pub width: u32,
    pub height: u32,
    pub lat: f64,
    pub lon: f64,
    pub poll_question: String,
    pub poll_options: Vec<String>,
    pub sha256: String,
    pub s3_bucket: String,
    pub s3_key: String,
    /// The ReplacingMergeTree version. 0 for a message as it was logged, and the
    /// archiver's ingest time on the row it enriches — so the enrichment always
    /// wins, and a message Telegram delivers a second time cannot blank it.
    pub version: u32,
}

impl Event {
    fn of(event: &str) -> Self {
        // Version 0, deliberately, not the ingest time: Telegram redelivers
        // updates after a reconnect, and an ingest-time version would make the
        // late copy of a message beat the archiver's enriched row and blank the
        // S3 columns off it. As it stands a redelivery is a no-op — same key,
        // same version — and only the archiver ever raises it.
        Self { event: event.to_string(), ..Self::default() }
    }

    pub fn send() -> Self {
        Self::of(SEND)
    }

    pub fn edit() -> Self {
        Self::of(EDIT)
    }

    pub fn delete() -> Self {
        Self::of(DELETE)
    }

    pub fn reaction() -> Self {
        Self::of(REACTION)
    }

    /// An ephemeral message's own event name: Telegram calls a new one "new", the
    /// log calls a new message "send".
    pub fn of_ephemeral(event: &str) -> Self {
        Self::of(if event == "new" { SEND } else { event })
    }

    /// The same message row again, carrying what the archiver learned about its
    /// file. Newer `version`, same key: it replaces the row it enriches.
    pub fn archived(&self, sha256: String, bucket: String, key: String, size: u64) -> Self {
        Self {
            sha256,
            s3_bucket: bucket,
            s3_key: key,
            size,
            version: now(),
            ..self.clone()
        }
    }
}

fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or_default()
}

#[derive(Row, Serialize)]
pub struct AdminAction {
    pub date: u32,
    pub event_id: u64,
    pub chat_id: u64,
    pub action_type: String,
    pub user_id: u64,
    pub message: String,
    pub log_output: String,
    pub message_id: u32,
    pub topic_id: u32,
    pub prev_value: String,
    pub new_value: String,
    pub usernames: Vec<String>,
    pub chat_usernames: Vec<String>,
    pub chat_title: String,
    pub user_title: String,
    pub target_user_id: u64,
    pub target_user_title: String,
    pub user_is_admin: bool,
}

#[derive(Row, Serialize)]
pub struct TelegramSession {
    pub hash: i64,
    pub device_model: String,
    pub platform: String,
    pub system_version: Option<String>,
    pub app_name: String,
    pub app_version: Option<String>,
    pub ip: Option<String>,
    pub country: String,
    pub region: String,
    pub date_created: u32,
    pub date_active: u32,
    pub updated_at: u32,
    pub client_id: u64,
}