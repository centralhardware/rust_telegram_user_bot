//! `lols_status`: lols.bot banlist checks of the users the bot has seen.
//! Used by the process-wide checker in `crate::lols`, not through the
//! per-account [`Db`](crate::db::Db) trait.

use super::*;
use crate::lols::{Known, LolsStatus};

impl ClickhouseDb {
    /// The latest check of every user ever checked.
    pub async fn lols_known(&self) -> DbResult<Vec<Known>> {
        Ok(self
            .ch
            .query(
                "SELECT user_id, \
                        argMax(lols_status.banned, lols_status.checked_at) AS banned, \
                        max(lols_status.checked_at) AS checked_at \
                 FROM lols_status GROUP BY user_id",
            )
            .fetch_all::<Known>()
            .await?)
    }

    pub async fn write_lols_status(&self, rows: &[LolsStatus]) -> DbResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        Ok(insert_rows(&self.ch, "lols_status", rows).await?)
    }
}
