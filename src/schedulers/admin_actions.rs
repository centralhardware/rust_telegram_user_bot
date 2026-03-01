use grammers_client::Client;
use grammers_tl_types as tl;
use log::{error, info};
use std::time::Duration;

use crate::db::AdminAction;

pub fn start(client: Client, client_id: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = log_admin_actions(&client, client_id).await {
                error!("Failed to fetch admin actions: {:?}", e);
            }
        }
    });
}

fn action_type_name(action: &tl::enums::ChannelAdminLogEventAction) -> String {
    let dbg = format!("{:?}", action);
    dbg.split(&['(', ' '][..]).next().unwrap_or(&dbg).to_string()
}

fn message_text(msg: &tl::enums::Message) -> String {
    match msg {
        tl::enums::Message::Message(m) => m.message.clone(),
        _ => String::new(),
    }
}

fn format_log_output(action: &tl::enums::ChannelAdminLogEventAction, user_title: &str) -> String {
    match action {
        tl::enums::ChannelAdminLogEventAction::EditMessage(a) => {
            let prev = message_text(&a.prev_message);
            let new = message_text(&a.new_message);
            if prev == new {
                return String::new();
            }
            similar::TextDiff::from_lines(&prev, &new)
                .unified_diff()
                .missing_newline_hint(false)
                .to_string()
        }
        tl::enums::ChannelAdminLogEventAction::DeleteMessage(a) => {
            message_text(&a.message)
        }
        tl::enums::ChannelAdminLogEventAction::ParticipantJoin => {
            format!("{} joined", user_title)
        }
        tl::enums::ChannelAdminLogEventAction::ParticipantLeave => {
            format!("{} left", user_title)
        }
        _ => String::new(),
    }
}

fn action_message_json(action: &tl::enums::ChannelAdminLogEventAction) -> String {
    serde_json::to_string(action).unwrap_or_default()
}

fn extract_user_info(
    users: &[tl::enums::User],
    user_id: i64,
) -> (String, Vec<String>) {
    for u in users {
        let tl::enums::User::User(user) = u else { continue };
        if user.id == user_id {
            let title = match (&user.first_name, &user.last_name) {
                (Some(first), Some(last)) if !last.is_empty() => format!("{} {}", first, last),
                (Some(first), _) => first.clone(),
                _ => String::new(),
            };
            let mut usernames = Vec::new();
            if let Some(ref username) = user.username {
                usernames.push(username.clone());
            }
            if let Some(ref unames) = user.usernames {
                for un in unames {
                    let tl::enums::Username::Username(u) = un;
                    if u.active {
                        usernames.push(u.username.clone());
                    }
                }
            }
            return (title, usernames);
        }
    }
    (String::new(), Vec::new())
}


async fn resolve_channel(
    client: &Client,
    chat_id: i64,
) -> Result<grammers_client::peer::Peer, Box<dyn std::error::Error>> {
    let input_peer = tl::types::InputPeerChannel {
        channel_id: chat_id,
        access_hash: 0,
    };
    Ok(client.resolve_peer(input_peer).await?)
}

async fn get_last_event_id(
    chat_id: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let max_id: u64 = crate::db::clickhouse()
        .query("SELECT max(event_id) FROM admin_actions2 WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_one()
        .await
        .unwrap_or(0);
    Ok(max_id)
}

async fn log_admin_actions(
    client: &Client,
    _client_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let ch = crate::db::clickhouse();

    let chat_ids_str = std::env::var("TELEGRAM_CHAT_IDS")?;
    let chat_ids: Vec<i64> = chat_ids_str
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    if chat_ids.is_empty() {
        return Ok(());
    }

    for chat_id in &chat_ids {
        let peer = match resolve_channel(client, *chat_id).await {
            Ok(r) => r,
            Err(e) => {
                error!("Cannot resolve channel {}: {:?}", chat_id, e);
                continue;
            }
        };
        let peer_ref = match peer.to_ref().await {
            Some(r) => r,
            None => {
                error!("Cannot get peer ref for channel {}", chat_id);
                continue;
            }
        };
        let channel_title = peer.name().unwrap_or("unknown").to_string();
        let channel_usernames: Vec<String> = {
            let mut unames = Vec::new();
            if let Some(u) = peer.username() {
                unames.push(u.to_string());
            }
            for u in peer.usernames() {
                unames.push(u.to_string());
            }
            unames
        };

        let chat_id_u64 = *chat_id as u64;
        let min_id = get_last_event_id(chat_id_u64).await? as i64;
        let mut max_id: i64 = 0;
        let mut total_inserted: usize = 0;
        let mut new_last_id: u64 = 0;

        loop {
            let input_channel: tl::enums::InputChannel = peer_ref.into();

            let tl::enums::channels::AdminLogResults::Results(result) = client
                .invoke(&tl::functions::channels::GetAdminLog {
                    channel: input_channel,
                    q: String::new(),
                    events_filter: None,
                    admins: None,
                    max_id,
                    min_id,
                    limit: 100,
                })
                .await?;

            if result.events.is_empty() {
                break;
            }

            let mut insert = ch.insert::<AdminAction>("admin_actions2").await?;

            for event in &result.events {
                let tl::enums::ChannelAdminLogEvent::Event(ev) = event;

                let (user_title, usernames) = extract_user_info(&result.users, ev.user_id);

                let log = &AdminAction {
                    date: ev.date as u32,
                    event_id: ev.id as u64,
                    chat_id: chat_id_u64,
                    action_type: action_type_name(&ev.action),
                    user_id: ev.user_id as u64,
                    message: action_message_json(&ev.action),
                    log_output: format_log_output(&ev.action, &user_title),
                    usernames,
                    chat_usernames: channel_usernames.clone(),
                    chat_title: channel_title.clone(),
                    user_title,
                };

                info!(
                    "admin    {:>12} {:<25} {:<20} {:<20}\n{}",
                    log.event_id,
                    &log.chat_title.chars().take(25).collect::<String>(),
                    &log.action_type.chars().take(20).collect::<String>(),
                    &log.user_title.chars().take(20).collect::<String>(),
                    log.log_output,
                );

                insert
                    .write(log)
                    .await?;
            }

            insert.end().await?;

            let (batch_min, batch_max) = result.events.iter().fold((i64::MAX, 0u64), |(min, max), e| {
                let tl::enums::ChannelAdminLogEvent::Event(ev) = e;
                (min.min(ev.id), max.max(ev.id as u64))
            });

            total_inserted += result.events.len();
            if batch_max > new_last_id {
                new_last_id = batch_max;
            }

            if result.events.len() < 100 {
                break;
            }

            max_id = batch_min;
        }

        if total_inserted > 0 {
            info!("[{}] Inserted {} entries. Last ID: {}", channel_title, total_inserted, new_last_id);
        }
    }

    Ok(())
}
