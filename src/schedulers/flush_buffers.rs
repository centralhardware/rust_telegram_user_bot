use std::time::Duration;

use crate::db;

pub async fn flush_all() {
    let events = db::EVENTS_BUF.flush().await;
    if events > 0 {
        log::info!("flushed events: {events}");
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
