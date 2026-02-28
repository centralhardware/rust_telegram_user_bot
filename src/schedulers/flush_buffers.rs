use std::time::Duration;

use crate::db;

pub fn start() {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let incoming = db::INCOMING_BUF.flush().await;
            let edited = db::EDITED_BUF.flush().await;
            let deleted = db::DELETED_BUF.flush().await;
            if incoming + edited + deleted > 0 {
                log::info!("flushed incoming: {incoming}, edited: {edited}, deleted: {deleted}");
            }
        }
    });
}
