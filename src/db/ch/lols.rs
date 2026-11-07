//! `lols_requests`: every HTTP request the spam checker makes to lols.bot.

use super::*;
use crate::lols::LolsRequest;

impl ClickhouseDb {
    pub async fn write_lols_request(&self, row: &LolsRequest) -> DbResult<()> {
        Ok(insert_rows(&self.ch, "lols_requests", std::slice::from_ref(row)).await?)
    }
}
