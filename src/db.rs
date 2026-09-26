//! What the bot keeps in its database, and the [`Db`] trait every query goes
//! through. [`ch::ClickhouseDb`] is the real one; tests use a fake.

use std::collections::HashSet;

use async_trait::async_trait;
use clickhouse::Row;
use serde::{Deserialize, Serialize};

use crate::state::peer_names::PeerNames;
use crate::state::poll_info::PollInfo;

pub mod ch;
pub mod migrate;
pub mod session;
#[cfg(test)]
pub mod fake;

pub type DbResult<T> = anyhow::Result<T>;

/// The Buffer table in front of `events_log` (migration 040). Everything the
/// bot writes goes here and everything it reads back comes from here: ClickHouse
/// holds the rows in memory and writes them down as one part a minute, and a
/// SELECT on a Buffer table reads the buffer and the destination both, so a
/// message logged a moment ago answers immediately.
///
/// Readers that are not the bot — Grafana, the aggregates, anything ad hoc —
/// query `events_log` and are at most a minute behind.
pub const EVENTS: &str = "events_log_buffer";

/// Every read and write the bot makes. A lookup that fails answers as if
/// nothing were found, unless the caller needs to tell the two apart, in which
/// case it returns a `DbResult`.
#[async_trait]
pub trait Db: Send + Sync {
    /// Log events into the Buffer, which is memory, so this is cheap and the
    /// rows are visible to the next lookup straight away. One insert, retried
    /// a few times as a whole to ride out a short hiccup.
    async fn log_events(&self, events: &[Event]);

    /// [`Db::log_events`] for one event.
    async fn log_event(&self, event: Event) {
        self.log_events(std::slice::from_ref(&event)).await
    }

    /// A backfill batch, straight into `events_log`: it is history, not
    /// something the next lookup is waiting on.
    async fn write_backfill(&self, events: &[Event]) -> DbResult<()>;

    /// The text a message stands at now — the last edit if there was one, the
    /// sent text otherwise — and its chat's title.
    async fn find_message(&self, chat_id: i64, message_id: i64) -> MessageInfo;

    /// What the log knows about every message in a deletion, in one query: a
    /// chat cleared or a batch deleted names a hundred ids at a time, and a
    /// round trip per id holds up every other chat on the same worker. The name
    /// comes from `peer_names`, so a sender renamed since is named as they are
    /// now.
    ///
    /// `channel` is the chat Telegram named. It names none for a private chat or
    /// a basic group, but outside channels message ids are unique per account,
    /// so the send rows name it: the one chat -- a user or a basic group, never
    /// a channel -- with a message of that id. A message the log never saw is
    /// not returned.
    async fn find_deleted(&self, channel: Option<i64>, message_ids: &[i64]) -> Vec<DeletedMessage>;

    /// The message a reply answers, as the log has it. `chat_id` is the chat
    /// the *replied-to* message lives in, which is not the answering message's
    /// chat when it quotes another.
    async fn find_target(&self, chat_id: i64, message_id: i64) -> ReplyTarget;

    /// A replied-to message's send row, for the reply line above a message.
    async fn find_reply_row(&self, chat_id: i64, message_id: i64) -> Option<ReplyRow>;

    /// Whether the log holds this message — sent, or a service message. An
    /// ephemeral id names a different message entirely and never answers.
    async fn message_exists(&self, chat_id: i64, message_id: i64) -> bool;

    /// Which of these message ids the log already holds.
    async fn known_ids(&self, chat_id: i64, ids: &[i64]) -> DbResult<HashSet<i64>>;

    /// The oldest message id the log holds for a chat, 0 when it holds none.
    async fn oldest_logged_id(&self, chat_id: i64) -> DbResult<i64>;

    /// Every chat id the log holds a message for.
    async fn logged_chat_ids(&self) -> DbResult<HashSet<i64>>;

    /// The title and usernames a chat last went by in the log.
    async fn last_chat_name(&self, chat_id: i64) -> Option<(String, Vec<String>)>;

    /// The poll with this id, from the send row that carried it.
    async fn find_poll(&self, poll_id: i64) -> Option<PollInfo>;

    /// The stored copy of a Telegram photo or document, if the archiver has
    /// one. A failed read is a miss.
    async fn find_media_file(&self, kind: &str, tg_id: i64) -> Option<MediaFile>;

    /// Write down where a Telegram file was stored.
    async fn remember_media_file(&self, file: MediaFile);

    /// The names last stored for a peer, by Bot API dialog id.
    async fn load_peer_names(&self, peer_id: i64) -> Option<PeerNames>;

    async fn write_peer_names(&self, names: &PeerNames);

    /// The newest admin-log event already stored for a chat, 0 for none.
    async fn last_admin_event_id(&self, chat_id: u64) -> u64;

    async fn write_admin_actions(&self, actions: &[AdminAction]) -> DbResult<()>;

    async fn write_user_sessions(&self, sessions: &[TelegramSession]) -> DbResult<()>;
}

pub use crate::events::EventKind;

#[derive(Default, Clone)]
pub struct MessageInfo {
    /// Whether the log has the message at all -- a send or an edit row. When
    /// it has not, the fields below are empty because nothing is known, not
    /// because the message was.
    pub logged: bool,
    pub message: String,
    /// The formatting and the buttons the message carries, as the columns of the
    /// same name hold them — an edit that changes only one of these changes
    /// nothing in `message`, and would otherwise pass for no edit at all.
    pub entities: Vec<crate::telegram::entities::Entity>,
    pub keyboard: Vec<crate::telegram::entities::Button>,
    pub chat_title: String,
}

/// A deleted message as the log has it: the chat it lived in, the text as it
/// last stood, the sender's name, and the chat's title.
#[derive(Row, Deserialize, Clone)]
pub struct DeletedMessage {
    pub chat_id: i64,
    pub message_id: i64,
    pub message: String,
    pub first_name: String,
    pub chat_title: String,
}

/// A Telegram file already stored in S3, from `media_files`.
#[derive(Row, Serialize, Deserialize, Clone)]
pub struct MediaFile {
    pub kind: String,
    pub tg_id: i64,
    pub sha256: String,
    pub s3_bucket: String,
    pub s3_key: String,
    pub size: u64,
}

/// A replied-to message's send row.
#[derive(Row, Deserialize, Default, Clone)]
pub struct ReplyRow {
    pub message: String,
    pub user_id: u64,
    pub chat_title: String,
    pub fwd_from_chat_id: i64,
    pub fwd_from_msg_id: i64,
}

/// What the log knows about the message a reply points at.
#[derive(Default, Clone)]
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
pub async fn resolve_reply(
    db: &dyn Db,
    chat_id: i64,
    reply: &mut crate::telegram::reply_target::ReplyInfo,
) -> u64 {
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

    let target = db.find_target(target_chat, id).await;

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
    pub entities: Vec<crate::telegram::entities::Entity>,
    /// The inline keyboard under the message, its rows flattened: each button
    /// names the row it sits in.
    pub keyboard: Vec<crate::telegram::entities::Button>,
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

#[derive(Row, Serialize, Clone)]
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

#[derive(Row, Serialize, Clone)]
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

