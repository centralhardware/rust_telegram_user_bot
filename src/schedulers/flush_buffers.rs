use std::time::Duration;

use crate::db;

pub async fn flush_all() {
    let events = db::EVENTS_BUF.flush().await;
    if events > 0 {
        log::info!("flushed events: {events}");
    }
    let names = crate::utils::peer_names::PEER_NAMES_BUF.flush().await;
    if names > 0 {
        log::info!("flushed peer names: {names}");
    }
    let peers = crate::clickhouse_session::PEER_CACHE_BUF.flush().await;
    if peers > 0 {
        log::info!("flushed peers: {peers}");
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
