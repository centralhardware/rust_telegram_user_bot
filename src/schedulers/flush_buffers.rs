use std::time::Duration;

use crate::db;

pub async fn flush_all() {
    let incoming = db::INCOMING_BUF.flush().await;
    let edited = db::EDITED_BUF.flush().await;
    let deleted = db::DELETED_BUF.flush().await;
    let media = db::MEDIA_BUF.flush().await;
    let ephemeral = db::EPHEMERAL_BUF.flush().await;
    if incoming + edited + deleted + media + ephemeral > 0 {
        log::info!(
            "flushed incoming: {incoming}, edited: {edited}, deleted: {deleted}, media: {media}, ephemeral: {ephemeral}"
        );
    }
}

pub fn start() {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            flush_all().await;
        }
    });
}
