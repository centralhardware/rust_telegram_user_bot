//! `user_sessions`: the account's other logged-in sessions.

use super::*;

impl ClickhouseDb {
    pub(super) async fn write_user_sessions(&self, sessions: &[TelegramSession]) -> DbResult<()> {
        Ok(self.insert("user_sessions", sessions).await?)
    }
}
