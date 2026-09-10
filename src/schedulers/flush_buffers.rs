use std::time::Duration;

use crate::db;

pub async fn flush_all() {
    let events = db::EVENTS_BUF.flush().await;
    let names = crate::utils::peer_names::PEER_NAMES_BUF.flush().await;
    let peers = crate::clickhouse_session::PEER_CACHE_BUF.flush().await;
    if events > 0 || names > 0 || peers > 0 {
        log::info!("flushed: events={events} peer_names={names} peers={peers}");
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
