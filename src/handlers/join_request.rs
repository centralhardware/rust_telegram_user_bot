use grammers_client::Client;
use grammers_tl_types as tl;
use log::info;
use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::db::{JoinRequest, clickhouse};

static SEEN: LazyLock<Mutex<HashSet<(i64, u64, i32)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn extract_user(users: &[tl::enums::User], user_id: i64) -> (String, String, Vec<String>) {
    for u in users {
        let tl::enums::User::User(user) = u else { continue };
        if user.id != user_id {
            continue;
        }
        let first = user.first_name.clone().unwrap_or_default();
        let last = user.last_name.clone().unwrap_or_default();
        let mut usernames = Vec::new();
        if let Some(ref u) = user.username {
            usernames.push(u.clone());
        }
        if let Some(ref unames) = user.usernames {
            for un in unames {
                let tl::enums::Username::Username(u) = un;
                if u.active && !usernames.contains(&u.username) {
                    usernames.push(u.username.clone());
                }
            }
        }
        return (first, last, usernames);
    }
    (String::new(), String::new(), Vec::new())
}

pub async fn handle_pending_join_requests(
    client: &Client,
    update: &tl::types::UpdatePendingJoinRequests,
    client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let (chat_id, input_peer) = match &update.peer {
        tl::enums::Peer::Channel(p) => (
            p.channel_id,
            tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
                channel_id: p.channel_id,
                access_hash: 0,
            }),
        ),
        tl::enums::Peer::Chat(p) => (
            p.chat_id,
            tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id: p.chat_id }),
        ),
        tl::enums::Peer::User(_) => return Ok(()),
    };

    let peer = client.resolve_peer(input_peer).await?;
    let peer_ref = match peer.to_ref().await {
        Some(r) => r,
        None => return Ok(()),
    };
    let resolved: tl::enums::InputPeer = peer_ref.into();
    let chat_title = peer.name().unwrap_or("").to_string();

    let tl::enums::messages::ChatInviteImporters::Importers(result) = client
        .invoke(&tl::functions::messages::GetChatInviteImporters {
            requested: true,
            subscription_expired: false,
            peer: resolved,
            link: None,
            q: None,
            offset_date: 0,
            offset_user: tl::enums::InputUser::Empty,
            limit: 200,
        })
        .await?;

    let mut rows: Vec<JoinRequest> = Vec::new();
    {
        let mut seen = SEEN.lock().await;
        for importer in &result.importers {
            let tl::enums::ChatInviteImporter::Importer(imp) = importer;
            if !imp.requested {
                continue;
            }
            let key = (chat_id, imp.user_id as u64, imp.date);
            if !seen.insert(key) {
                continue;
            }

            let about = imp.about.clone().unwrap_or_default();
            let (first_name, second_name, username) = extract_user(&result.users, imp.user_id);

            let display = if second_name.is_empty() {
                first_name.clone()
            } else {
                format!("{} {}", first_name, second_name)
            };
            let user_short: String = display.chars().take(10).collect();
            let chat_short: String = chat_title.chars().take(25).collect();
            info!(
                "\x1b[96m{:<8} {:>8} {:<25} \x1b[90m│\x1b[96m {:<10} \x1b[90m│\x1b[96m {}\x1b[0m",
                "join_req", imp.user_id, chat_short, user_short, about
            );

            rows.push(JoinRequest {
                date_time: imp.date as u32,
                chat_id,
                user_id: imp.user_id as u64,
                username,
                first_name,
                second_name,
                about,
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
