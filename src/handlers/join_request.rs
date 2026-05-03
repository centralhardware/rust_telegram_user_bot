use grammers_tl_types as tl;
use log::info;
use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::db::{JoinRequest, clickhouse};

static SEEN: LazyLock<Mutex<HashSet<(i64, u64)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub async fn handle_pending_join_requests(
    update: &tl::types::UpdatePendingJoinRequests,
    client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let chat_id = match &update.peer {
        tl::enums::Peer::Channel(p) => p.channel_id,
        tl::enums::Peer::Chat(p) => p.chat_id,
        tl::enums::Peer::User(_) => return Ok(()),
    };

    let now = chrono::Utc::now().timestamp() as u32;
    let mut rows: Vec<JoinRequest> = Vec::new();
    {
        let mut seen = SEEN.lock().await;
        for &uid in &update.recent_requesters {
            let user_id = uid as u64;
            if !seen.insert((chat_id, user_id)) {
                continue;
            }
            info!(
                "\x1b[96m{:<8} {:>12} chat {}\x1b[0m",
                "join_req", user_id, chat_id
            );
            rows.push(JoinRequest {
                date_time: now,
                chat_id,
                user_id,
                client_id,
            });
        }
    }

    if rows.is_empty() {
        return Ok(());
    }

    let mut insert = clickhouse().insert::<JoinRequest>("join_requests_log").await?;
    for row in &rows {
        insert.write(row).await?;
    }
    insert.end().await?;

    Ok(())
}
