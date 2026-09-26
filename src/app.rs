//! Everything the bot runs on, built once in `main` and handed down: the
//! Telegram client, the database, object storage, settings, and the small
//! caches that live as long as the process. Nothing here is reached through a
//! global, so a function's signature says what it needs, and a test can build
//! an `App` over a fake database.

use std::sync::Arc;

use grammers_client::Client;

use crate::db::session::ClickhouseSession;
use crate::db::Db;
use crate::handlers::backfill_chat::RunningBackfills;
use crate::handlers::media::MediaQueue;
use crate::handlers::polls::PollCounts;
use crate::s3::Storage;
use crate::state::admin_chats::AdminChats;
use crate::state::log_ignore::LogIgnore;
use crate::state::peer_names::WrittenNames;
use crate::state::topic::TopicNames;

pub struct App {
    pub tg: Client,
    pub db: Arc<dyn Db>,
    /// `None` when S3 is not configured, which leaves media archiving off.
    pub storage: Option<Storage>,
    /// The session the client runs on, kept so a backfill can ask it what it
    /// knows about a peer. `None` in tests.
    pub session: Option<Arc<ClickhouseSession>>,
    /// The logged-in account's own id.
    pub me: u64,
    /// Chats kept out of the console. Shared with the logger, which needs it
    /// before there is an `App`.
    pub ignored: Arc<LogIgnore>,
    pub admin_chats: AdminChats,
    pub media: MediaQueue,
    pub topics: TopicNames,
    pub names_written: WrittenNames,
    pub polls: PollCounts,
    pub backfills: RunningBackfills,
}

impl App {
    pub fn new(
        tg: Client,
        db: Arc<dyn Db>,
        storage: Option<Storage>,
        session: Option<Arc<ClickhouseSession>>,
        me: u64,
        ignored: Arc<LogIgnore>,
    ) -> Self {
        App {
            tg,
            db,
            storage,
            session,
            me,
            ignored,
            admin_chats: AdminChats::default(),
            media: MediaQueue::default(),
            topics: TopicNames::default(),
            names_written: WrittenNames::default(),
            polls: PollCounts::default(),
            backfills: RunningBackfills::default(),
        }
    }

    /// Whether a chat is kept out of the console.
    pub fn is_log_ignored(&self, chat_id: i64) -> bool {
        self.ignored.contains(chat_id)
    }

    /// An `App` over a [`FakeDb`](crate::db::fake::FakeDb) and a Telegram
    /// client that never connects: nothing runs its sender pool, so any call
    /// that would reach Telegram waits forever. Enough for everything that
    /// only reads the update in hand and the database.
    #[cfg(test)]
    pub fn for_tests(db: Arc<crate::db::fake::FakeDb>) -> Self {
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let pool = grammers_client::SenderPool::new(session, 1);
        App::new(
            Client::new(pool.handle),
            db,
            None,
            None,
            1,
            Arc::new(LogIgnore::default()),
        )
    }
}

#[cfg(test)]
mod tests {
    //! Handlers run against a fake database: what they write, and what they
    //! read back, without ClickHouse or Telegram.

    use std::sync::Arc;

    use super::App;
    use crate::db::fake::FakeDb;
    use crate::db::{Event, EventKind};

    fn app() -> (App, Arc<FakeDb>) {
        let db = Arc::new(FakeDb::default());
        (App::for_tests(Arc::clone(&db)), db)
    }

    #[tokio::test]
    async fn an_unpin_is_logged_for_every_message_it_names() {
        let (app, db) = app();
        crate::handlers::save_pinned(&app, 5, -1005, &[10, 11], false).await;

        let rows = db.events();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.event == EventKind::Unpin && !r.pinned && r.chat_id == 5));
        assert_eq!(rows.iter().map(|r| r.message_id).collect::<Vec<_>>(), [10, 11]);
    }

    #[tokio::test]
    async fn a_view_count_is_its_own_row() {
        let (app, db) = app();
        crate::handlers::save_views(&app, 7, 3, 120, 0).await;

        let [row] = db.events().try_into().ok().unwrap();
        assert_eq!(row.event, EventKind::Views);
        assert_eq!((row.chat_id, row.message_id, row.views, row.forwards), (7, 3, 120, 0));
    }

    /// Telegram threads a post's comments off the copy of the post in the
    /// discussion group, so a reply to that copy is a comment, not a reply.
    #[tokio::test]
    async fn a_reply_to_a_channel_post_copy_is_a_comment() {
        let (app, db) = app();
        db.events.lock().unwrap().push(Event {
            chat_id: 9,
            message_id: 100,
            user_id: 0,
            fwd_from_chat_id: 42,
            fwd_from_msg_id: 7,
            ..Event::of(EventKind::Send)
        });

        let mut reply = crate::telegram::reply_target::ReplyInfo {
            reply_to: 100,
            ..Default::default()
        };
        let user = crate::db::resolve_reply(&*app.db, 9, &mut reply).await;

        assert_eq!(user, 0);
        assert_eq!(reply.reply_to, 0);
        assert_eq!(reply.comment_to, 100);
    }

    #[tokio::test]
    async fn an_ordinary_reply_names_who_it_answers() {
        let (app, db) = app();
        db.events.lock().unwrap().push(Event {
            chat_id: 9,
            message_id: 100,
            user_id: 55,
            ..Event::of(EventKind::Send)
        });

        let mut reply = crate::telegram::reply_target::ReplyInfo {
            reply_to: 100,
            ..Default::default()
        };
        assert_eq!(crate::db::resolve_reply(&*app.db, 9, &mut reply).await, 55);
        assert_eq!(reply.reply_to, 100);
        assert_eq!(reply.comment_to, 0);
    }
}
