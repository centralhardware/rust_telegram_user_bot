//! `admin_actions2`: the admin logs of the chats this account administers.

use super::*;

impl ClickhouseDb {
    pub(super) async fn last_admin_event_id(&self, chat_id: u64) -> u64 {
        self.ch
            .query("SELECT max(event_id) FROM admin_actions2 WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one()
            .await
            .unwrap_or(0)
    }

    pub(super) async fn write_admin_actions(&self, actions: &[AdminAction]) -> DbResult<()> {
        Ok(self.insert("admin_actions2", actions).await?)
    }
}
