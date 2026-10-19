//! `admin_actions`: the admin logs of the chats this account administers.

use super::*;

impl ClickhouseDb {
    pub(super) async fn last_admin_event_id(&self, chat_id: u64) -> DbResult<u64> {
        Ok(self
            .ch
            .query("SELECT max(event_id) FROM admin_actions WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one()
            .await?)
    }

    pub(super) async fn write_admin_actions(&self, actions: &[AdminAction]) -> DbResult<()> {
        Ok(insert_rows(&self.ch, "admin_actions", actions).await?)
    }
}
