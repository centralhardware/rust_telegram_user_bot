//! An in-memory [`Db`] for tests: every write lands in a `Vec`, and the reads
//! answer from what was written, the way the Buffer table would.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use super::{
    AdminAction, Db, DbResult, DeletedMessage, Event, EventKind, MediaFile, MessageInfo, ReplyRow,
    ReplyTarget, TelegramSession,
};
use crate::state::peer_names::PeerNames;
use crate::state::poll_info::PollInfo;

#[derive(Default)]
pub struct FakeDb {
    pub events: Mutex<Vec<Event>>,
    pub peer_names: Mutex<HashMap<i64, PeerNames>>,
    pub media_files: Mutex<Vec<MediaFile>>,
    pub admin_actions: Mutex<Vec<AdminAction>>,
    pub sessions: Mutex<Vec<TelegramSession>>,
}

impl FakeDb {
    /// Every event written so far.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// The newest row of one of these kinds for a message.
    fn latest(&self, chat_id: i64, message_id: i64, kinds: &[EventKind]) -> Option<Event> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.chat_id == chat_id && e.message_id == message_id && kinds.contains(&e.event)
            })
            .max_by_key(|e| (e.event == EventKind::Edit, e.date_time))
            .cloned()
    }

    fn chat_title(&self, chat_id: i64) -> String {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.chat_id == chat_id && e.event == EventKind::Send && !e.chat_title.is_empty()
            })
            .max_by_key(|e| e.date_time)
            .map(|e| e.chat_title.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Db for FakeDb {
    async fn log_events(&self, events: &[Event]) {
        self.events.lock().unwrap().extend_from_slice(events);
    }

    async fn write_backfill(&self, events: &[Event]) -> DbResult<()> {
        self.log_events(events).await;
        Ok(())
    }

    async fn find_message(&self, chat_id: i64, message_id: i64) -> MessageInfo {
        let row = self.latest(chat_id, message_id, &[EventKind::Send, EventKind::Edit]);
        MessageInfo {
            logged: row.is_some(),
            message: row.as_ref().map(|r| r.message.clone()).unwrap_or_default(),
            entities: row.as_ref().map(|r| r.entities.clone()).unwrap_or_default(),
            keyboard: row.as_ref().map(|r| r.keyboard.clone()).unwrap_or_default(),
            chat_title: self.chat_title(chat_id),
        }
    }

    async fn find_deleted(&self, channel: Option<i64>, message_ids: &[i64]) -> Vec<DeletedMessage> {
        let mut found = Vec::new();
        for &id in message_ids {
            let send = self.events().into_iter().find(|e| {
                e.message_id == id
                    && e.event == EventKind::Send
                    && channel.is_none_or(|c| e.chat_id == c)
            });
            let Some(send) = send else { continue };
            let info = self.find_message(send.chat_id, id).await;
            let first_name = self
                .peer_names
                .lock()
                .unwrap()
                .get(&(send.user_id as i64))
                .map(|n| n.first_name.clone())
                .unwrap_or_default();
            found.push(DeletedMessage {
                chat_id: send.chat_id,
                message_id: id,
                message: info.message,
                first_name,
                chat_title: info.chat_title,
            });
        }
        found
    }

    async fn find_target(&self, chat_id: i64, message_id: i64) -> ReplyTarget {
        self.latest(chat_id, message_id, &[EventKind::Send])
            .map(|e| ReplyTarget {
                user_id: e.user_id,
                post_copy: e.user_id == 0 && e.fwd_from_chat_id != 0 && e.fwd_from_msg_id != 0,
            })
            .unwrap_or_default()
    }

    async fn find_reply_row(&self, chat_id: i64, message_id: i64) -> Option<ReplyRow> {
        self.latest(chat_id, message_id, &[EventKind::Send])
            .map(|e| ReplyRow {
                message: e.message,
                user_id: e.user_id,
                chat_title: e.chat_title,
                fwd_from_chat_id: e.fwd_from_chat_id,
                fwd_from_msg_id: e.fwd_from_msg_id,
            })
    }

    async fn message_exists(&self, chat_id: i64, message_id: i64) -> bool {
        self.events.lock().unwrap().iter().any(|e| {
            e.chat_id == chat_id
                && e.message_id == message_id
                && matches!(e.event, EventKind::Send | EventKind::Service)
                && !e.ephemeral
        })
    }

    async fn known_ids(&self, chat_id: i64, ids: &[i64]) -> DbResult<HashSet<i64>> {
        let mut known = HashSet::new();
        for &id in ids {
            if self.message_exists(chat_id, id).await {
                known.insert(id);
            }
        }
        Ok(known)
    }

    async fn oldest_logged_id(&self, chat_id: i64) -> DbResult<i64> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.chat_id == chat_id && e.event == EventKind::Send)
            .map(|e| e.message_id)
            .min()
            .unwrap_or(0))
    }

    async fn logged_chat_ids(&self) -> DbResult<HashSet<i64>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !e.ephemeral)
            .map(|e| e.chat_id)
            .collect())
    }

    async fn last_chat_name(&self, chat_id: i64) -> Option<(String, Vec<String>)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.chat_id == chat_id && e.event == EventKind::Send && !e.chat_title.is_empty()
            })
            .max_by_key(|e| e.date_time)
            .map(|e| (e.chat_title.clone(), e.chat_usernames.clone()))
    }

    async fn find_poll(&self, poll_id: i64) -> Option<PollInfo> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.poll_id == poll_id
                    && matches!(e.event, EventKind::Send | EventKind::Edit)
                    && !e.poll_question.is_empty()
            })
            .max_by_key(|e| e.date_time)
            .map(|e| PollInfo {
                chat_id: e.chat_id,
                chat_title: e.chat_title.clone(),
                message_id: e.message_id,
                question: e.poll_question.clone(),
                options: e.poll_options.clone(),
            })
    }

    async fn find_media_file(&self, kind: &str, tg_id: i64) -> Option<MediaFile> {
        self.media_files
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|f| f.kind == kind && f.tg_id == tg_id)
            .cloned()
    }

    async fn remember_media_file(&self, file: MediaFile) {
        self.media_files.lock().unwrap().push(file);
    }

    async fn load_peer_names(&self, peer_id: i64) -> Option<PeerNames> {
        self.peer_names.lock().unwrap().get(&peer_id).cloned()
    }

    async fn write_peer_names(&self, names: &PeerNames) {
        self.peer_names
            .lock()
            .unwrap()
            .insert(names.peer_id, names.clone());
    }

    async fn last_admin_event_id(&self, chat_id: u64) -> u64 {
        self.admin_actions
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.chat_id == chat_id)
            .map(|a| a.event_id)
            .max()
            .unwrap_or(0)
    }

    async fn write_admin_actions(&self, actions: &[AdminAction]) -> DbResult<()> {
        self.admin_actions
            .lock()
            .unwrap()
            .extend_from_slice(actions);
        Ok(())
    }

    async fn write_user_sessions(&self, sessions: &[TelegramSession]) -> DbResult<()> {
        self.sessions.lock().unwrap().extend_from_slice(sessions);
        Ok(())
    }
}
