use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static CLICKHOUSE: LazyLock<Client> = LazyLock::new(|| {
    Client::default()
        .with_url(std::env::var("CLICKHOUSE_URL").expect("CLICKHOUSE_URL not set"))
        .with_user(std::env::var("CLICKHOUSE_USER").expect("CLICKHOUSE_USER not set"))
        .with_password(std::env::var("CLICKHOUSE_PASSWORD").expect("CLICKHOUSE_PASSWORD not set"))
        .with_database(std::env::var("CLICKHOUSE_DATABASE").expect("CLICKHOUSE_DATABASE not set"))
        // Many small writes — one per event, one per update position — so the
        // server batches them into parts. Not waiting for the flush: an insert
        // returns once the server has the rows, and only a connection or
        // parsing failure comes back as an error.
        .with_setting("async_insert", "1")
        .with_setting("wait_for_async_insert", "0")
});

/// Build the client now, so a missing setting stops the bot at startup rather
/// than at the first write.
pub fn init() {
    LazyLock::force(&CLICKHOUSE);
}

pub fn clickhouse() -> &'static Client {
    &CLICKHOUSE
}

/// Write rows to a table. Nothing is queued here: `async_insert` on the client
/// (set above) means the server holds the rows and decides when they become a part.
pub async fn insert_rows<T>(table: &str, rows: &[T]) -> Result<(), clickhouse::error::Error>
where
    T: Serialize + Send + 'static,
    for<'a> T: Row<Value<'a> = T>,
{
    let mut insert = clickhouse().insert::<T>(table).await?;
    for row in rows {
        insert.write(row).await?;
    }
    insert.end().await
}

/// The Buffer table in front of `events_log` (migration 040). Everything the
/// bot writes goes here and everything it reads back comes from here: ClickHouse
/// holds the rows in memory and writes them down as one part a minute, and a
/// SELECT on a Buffer table reads the buffer and the destination both, so a
/// message logged a moment ago answers immediately.
///
/// Readers that are not the bot — Grafana, the aggregates, anything ad hoc —
/// query `events_log` and are at most a minute behind.
pub const EVENTS: &str = "events_log_buffer";

/// Log one event. It lands in the Buffer, which is memory, so this is cheap and
/// the row is visible to the next lookup without waiting for a part to be
/// written.
///
/// A failed write is retried a few times, which rides out a short hiccup.
pub async fn log_event(event: Event) {
    log_events(std::slice::from_ref(&event)).await
}

/// [`log_event`] for several rows at once: one insert, retried as a whole.
pub async fn log_events(events: &[Event]) {
    let mut delay = std::time::Duration::from_millis(500);
    for attempt in 1..=INSERT_ATTEMPTS {
        match insert_rows(EVENTS, events).await {
            Ok(()) => return,
            Err(e) if attempt == INSERT_ATTEMPTS => {
                log::error!("insert into {EVENTS}: {e}");
            }
            Err(e) => {
                log::warn!("insert into {EVENTS} (attempt {attempt}): {e}");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
}

const INSERT_ATTEMPTS: u32 = 3;

pub use crate::events::EventKind;

pub struct MessageInfo {
    /// Whether the log has the message at all -- a send or an edit row. When
    /// it has not, the fields below are empty because nothing is known, not
    /// because the message was.
    pub logged: bool,
    pub message: String,
    /// The formatting and the buttons the message carries, as the columns of the
    /// same name hold them — an edit that changes only one of these changes
    /// nothing in `message`, and would otherwise pass for no edit at all.
    pub entities: Vec<crate::utils::entities::Entity>,
    pub keyboard: Vec<crate::utils::entities::Button>,
    pub chat_title: String,
}

/// The body of a message as the log has it, read back for an edit.
#[derive(Row, Deserialize, Default)]
struct BodyRow {
    message: String,
    entities: Vec<crate::utils::entities::Entity>,
    keyboard: Vec<crate::utils::entities::Button>,
}

/// Find message info by chat_id + message_id: the text as it stands now — the
/// last edit if there was one, the sent text otherwise.
///
/// Straight from ClickHouse: the read goes to the Buffer table, which answers
/// out of its own memory and the table underneath both, so a message logged a
/// moment ago is already there to be read back.
pub async fn find_message(chat_id: i64, message_id: i64) -> MessageInfo {
    let body = clickhouse()
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

    let chat_title = clickhouse()
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

/// A deleted message as the log has it: the chat it lived in, the text as it
/// last stood, the sender's name, and the chat's title.
#[derive(Row, Deserialize)]
pub struct DeletedMessage {
    pub chat_id: i64,
    pub message_id: i64,
    pub message: String,
    pub first_name: String,
    pub chat_title: String,
}

/// What the log knows about every message in a deletion, in one query: a chat
/// cleared or a batch deleted names a hundred ids at a time, and a round trip
/// per id holds up every other chat on the same worker. The name comes from
/// `peer_names`, so a sender renamed since is named as they are now.
///
/// `channel` is the chat Telegram named. It names none for a private chat or a
/// basic group, but outside channels message ids are unique per account, so
/// the send rows name it: the one chat -- a user or a basic group, never a
/// channel -- with a message of that id. A message the log never saw is not
/// returned.
pub async fn find_deleted(channel: Option<i64>, message_ids: &[i64]) -> Vec<DeletedMessage> {
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
    let mut query = clickhouse().query(&sql).bind(EventKind::Edit).bind(EventKind::Send);
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
            log::warn!("looking up deleted messages {message_ids:?}: {e}");
            Vec::new()
        })
}

/// A Telegram file already stored in S3, from `media_files`.
#[derive(Row, Serialize, Deserialize)]
pub struct MediaFile {
    pub kind: String,
    pub tg_id: i64,
    pub sha256: String,
    pub s3_bucket: String,
    pub s3_key: String,
    pub size: u64,
}

/// The stored copy of a Telegram photo or document, if the archiver has one.
/// A failed read is a miss: the file is downloaded as if never seen.
pub async fn find_media_file(kind: &str, tg_id: i64) -> Option<MediaFile> {
    clickhouse()
        .query(
            "SELECT ?fields FROM media_files FINAL \
             WHERE kind = ? AND tg_id = ? LIMIT 1",
        )
        .bind(kind)
        .bind(tg_id)
        .fetch_optional::<MediaFile>()
        .await
        .unwrap_or_else(|e| {
            log::warn!("media_files lookup for {kind} {tg_id}: {e}");
            None
        })
}

/// Write down where a Telegram file was stored, for the next time it is posted.
pub async fn remember_media_file(file: MediaFile) {
    if let Err(e) = insert_rows("media_files", std::slice::from_ref(&file)).await {
        log::warn!("media_files insert for {} {}: {e}", file.kind, file.tg_id);
    }
}

/// What the log knows about the message a reply points at.
#[derive(Default)]
pub struct ReplyTarget {
    /// Who sent it, 0 when the message is older than the log, was never seen,
    /// or was posted by a channel rather than a user.
    pub user_id: u64,
    /// Whether it is the copy of a channel post that Telegram auto-forwards
    /// into the linked discussion group — the root every comment on that post
    /// hangs off. Such a copy is sent by the channel (no user) and carries the
    /// post's own id in `fwd_from_msg_id`.
    pub post_copy: bool,
}

/// The message a reply answers, as the log has it. `chat_id` is the chat the
/// *replied-to* message lives in, which is not the answering message's chat
/// when it quotes another.
pub async fn find_target(chat_id: i64, message_id: i64) -> ReplyTarget {
    clickhouse()
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

/// Settle what a message replies to, and who sent that: the target is looked up
/// in the chat it actually lives in — the quoted chat when the reply quotes
/// another one, this chat otherwise.
///
/// A comment on a channel post is **not** a reply. Telegram builds the comment
/// section out of replies to the copy of the post in the discussion group, so
/// every top-level comment names that copy the way a reply names its target;
/// counting them as replies makes each post look like a conversation with
/// itself. When the target turns out to be that copy, the reply is cleared and
/// the comment is logged as the plain message it is. A comment answering
/// *another comment* points at that comment, not at the post, and stays a reply.
pub async fn resolve_reply(chat_id: i64, reply: &mut crate::utils::reply_target::ReplyInfo) -> u64 {
    let id = match reply.reply_to {
        0 => return 0,
        id => id as i64,
    };
    let quoted_chat = reply.reply_to_chat_id != 0;
    let target_chat = if quoted_chat {
        reply.reply_to_chat_id
    } else {
        chat_id
    };

    let target = find_target(target_chat, id).await;

    // Only in the chat the message was posted in: a quote of another chat names
    // a post over there deliberately, and is a reply whatever it points at.
    if target.post_copy && !quoted_chat {
        reply.comment_to = reply.reply_to;
        reply.reply_to = 0;
        return 0;
    }

    target.user_id
}

/// One `events_log` row. Built through `Event::of(kind)`, which names the
/// event and leaves every column the event does not use empty, or from one of
/// the typed rows in `events` for the kinds that fill only a few columns.
///
/// A message is one row whatever it carries: the text representation in
/// `message`, the message object itself in `raw`, and — when it carries media —
/// what that media is in the `media_*` columns. A file archived to S3 is a row
/// of its own — `file_uploaded`, carrying `sha256` / `s3_*` and the identity of
/// the message it belongs to — so no row of this table is ever written twice.
#[derive(Row, Serialize, Default, Clone)]
pub struct Event {
    pub date_time: u32,
    pub event: EventKind,
    pub chat_id: i64,
    pub chat_title: String,
    pub message_id: i64,
    /// What the sender wrote, as they wrote it: no formatting markers, no buttons
    /// glued underneath. When there is no text — media, a service action — it is
    /// the description of that instead, as it always was.
    pub message: String,
    /// The formatting Telegram draws over `message` — what it is, the span it
    /// covers in UTF-16 code units, and the one thing it carries besides. Kept
    /// beside the text rather than baked into it, so a reader can render it, or
    /// ignore it and read the text.
    pub entities: Vec<crate::utils::entities::Entity>,
    /// The inline keyboard under the message, its rows flattened: each button
    /// names the row it sits in.
    pub keyboard: Vec<crate::utils::entities::Button>,
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
    /// The chat `reply_to` belongs to, 0 when that is this chat — every ordinary
    /// reply. Telegram also lets a message quote one from *another* chat, and
    /// then `reply_to` is an id over there: joining it onto this chat's messages
    /// would find nothing, or the unrelated message carrying the same id.
    pub reply_to_chat_id: i64,
    /// The passage of the replied-to message the sender selected, empty when
    /// they quoted nothing. For a quote out of a chat the account does not see,
    /// this is the only trace of what was quoted.
    pub quote_text: String,
    /// The channel post this message comments on, 0 when it comments on none.
    /// A comment is not a reply: Telegram threads a post's comment section off
    /// the copy of the post in the discussion group, and this names that copy
    /// rather than leaving it to look like the message being answered.
    pub comment_to: u64,
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
    /// The id of the service message that announced the action, for a 'service'
    /// row — whose `message_id` is the message the action was performed on. 0
    /// everywhere else.
    pub service_message_id: i64,
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
    /// Edits: the inline word diff against the text this edit replaced, rendered
    /// as HTML — the same marking the console line shows in ANSI. That text is
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
    /// Telegram's own id for the poll, on both the message carrying it and the
    /// 'poll' rows that follow: a results update names the message only
    /// sometimes, and this is the link back when it does not.
    pub poll_id: i64,
    /// A 'poll' row: the voters per option after the change, keyed by the option
    /// identifier — the wording is in `poll_options` on the send row.
    pub poll_results: Vec<(String, u32)>,
    pub poll_total_voters: u32,
    /// A 'views' row: a channel post's counters. Telegram reports each on its
    /// own, so the one this update did not carry is 0.
    pub views: u32,
    pub forwards: u32,
    pub sha256: String,
    pub s3_bucket: String,
    pub s3_key: String,
}

impl Event {
    /// A row of this kind with every column empty, for the caller to fill.
    pub fn of(event: EventKind) -> Self {
        // No version column any more (migration 039): nothing rewrites a row, so
        // the only thing ReplacingMergeTree still collapses is a redelivery of
        // the same event after a reconnect — the same row on the same key, where
        // it does not matter which copy survives.
        Self {
            event,
            ..Self::default()
        }
    }

    /// What the archiver learned about a message's file, as an event of its own.
    ///
    /// It carries the identity of the message — chat, id, topic, whether it is
    /// ephemeral — and the file, and nothing else: the text, the sender, the
    /// chat's title and the raw update are on the send row this one points at,
    /// and repeating them would be storing the same message twice. `date_time` is the upload, not
    /// the message: this row says when the file reached S3.
    pub fn file_uploaded(&self, sha256: String, bucket: String, key: String, size: u64) -> Self {
        Self {
            date_time: now(),
            event: EventKind::FileUploaded,
            chat_id: self.chat_id,
            message_id: self.message_id,
            topic_id: self.topic_id,
            topic_name: self.topic_name.clone(),
            ephemeral: self.ephemeral,
            receiver_id: self.receiver_id,
            media_type: self.media_type.clone(),
            file_name: self.file_name.clone(),
            mime_type: self.mime_type.clone(),
            // The bytes actually stored, which is what Telegram reported only
            // when it reported anything at all.
            size,
            sha256,
            s3_bucket: bucket,
            s3_key: key,
            ..Self::default()
        }
    }
}

pub fn now() -> u32 {
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
#[cfg(test)]
mod tests {
    use super::*;

    fn send_with_media() -> Event {
        Event {
            chat_id: -100,
            chat_title: "chat".to_string(),
            message_id: 7,
            message: "look at this".to_string(),
            raw: "{}".to_string(),
            user_id: 42,
            topic_id: 3,
            topic_name: "topic".to_string(),
            media_type: "photo".to_string(),
            size: 1024,
            ..Event::of(EventKind::Send)
        }
    }

    #[test]
    fn the_archiver_writes_its_own_event_not_the_send_row_again() {
        let uploaded = send_with_media().file_uploaded(
            "abc".to_string(),
            "bucket".to_string(),
            "ab/c/abc.jpg".to_string(),
            2048,
        );

        assert_eq!(uploaded.event, EventKind::FileUploaded);
        // The message it belongs to, so the row can be joined back onto its send.
        assert_eq!(uploaded.chat_id, -100);
        assert_eq!(uploaded.message_id, 7);
        assert_eq!(uploaded.topic_id, 3);
        // The file, at the size actually stored.
        assert_eq!(uploaded.s3_key, "ab/c/abc.jpg");
        assert_eq!(uploaded.size, 2048);
        // Nothing the send row already carries.
        assert!(uploaded.message.is_empty());
        assert!(uploaded.chat_title.is_empty());
        assert!(uploaded.raw.is_empty());
        assert_eq!(uploaded.user_id, 0);
    }
}

