use std::time::Duration;

use crate::db;

pub fn start() {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            db::INCOMING_BUF.flush().await;
            db::EDITED_BUF.flush().await;
            db::DELETED_BUF.flush().await;
        }
    });
}
