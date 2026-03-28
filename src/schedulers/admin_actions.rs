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

fn participant_user_id(p: &tl::enums::ChannelParticipant) -> Option<i64> {
    use tl::enums::ChannelParticipant::*;
    match p {
        Participant(p) => Some(p.user_id),
        ParticipantSelf(p) => Some(p.user_id),
        Creator(p) => Some(p.user_id),
        Admin(p) => Some(p.user_id),
        Banned(p) => match &p.peer {
            tl::enums::Peer::User(u) => Some(u.user_id),
            _ => None,
        },
        Left(p) => match &p.peer {
            tl::enums::Peer::User(u) => Some(u.user_id),
            _ => None,
        },
    }
}

fn participant_name(p: &tl::enums::ChannelParticipant, users: &[tl::enums::User]) -> String {
    participant_user_id(p)
        .map(|id| extract_user_info(users, id).0)
        .unwrap_or_default()
}

fn format_log_output(action: &tl::enums::ChannelAdminLogEventAction, user_title: &str, users: &[tl::enums::User]) -> String {
    use tl::enums::ChannelAdminLogEventAction::*;
    match action {
        ChangeTitle(a) => format!("title: {} -> {}", a.prev_value, a.new_value),
        ChangeAbout(a) => format!("about: {} -> {}", a.prev_value, a.new_value),
        ChangeUsername(a) => format!("username: {} -> {}", a.prev_value, a.new_value),
        ChangePhoto(_) => "photo changed".to_string(),
        ToggleInvites(a) => format!("invites: {}", if a.new_value { "enabled" } else { "disabled" }),
        ToggleSignatures(a) => format!("signatures: {}", if a.new_value { "enabled" } else { "disabled" }),
        UpdatePinned(_) => "message pinned/unpinned".to_string(),
        EditMessage(a) => {
            let prev = message_text(&a.prev_message);
            let new = message_text(&a.new_message);
            if prev == new {
                return String::new();
            }
            let diff = similar::TextDiff::from_lines(&prev, &new)
                .unified_diff()
                .missing_newline_hint(false)
                .to_string();
            crate::utils::diff::colorize_unified_diff(&diff, &prev, &new)
        }
        DeleteMessage(a) => message_text(&a.message),
        ParticipantJoin => format!("{} joined", user_title),
        ParticipantLeave => format!("{} left", user_title),
        ParticipantInvite(a) => format!("{} invited", participant_name(&a.participant, users)),
        ParticipantToggleBan(a) => format!("{} ban toggled", participant_name(&a.new_participant, users)),
        ParticipantToggleAdmin(a) => format!("{} admin toggled", participant_name(&a.new_participant, users)),
        ChangeStickerSet(_) => "sticker set changed".to_string(),
        TogglePreHistoryHidden(a) => format!("pre-history: {}", if a.new_value { "hidden" } else { "visible" }),
        DefaultBannedRights(_) => "default banned rights changed".to_string(),
        StopPoll(_) => "poll stopped".to_string(),
        ChangeLinkedChat(a) => format!("linked chat: {} -> {}", a.prev_value, a.new_value),
        ChangeLocation(_) => "location changed".to_string(),
        ToggleSlowMode(a) => format!("slow mode: {}s -> {}s", a.prev_value, a.new_value),
        StartGroupCall(_) => "group call started".to_string(),
        DiscardGroupCall(_) => "group call ended".to_string(),
        ParticipantMute(_) => format!("{} muted in call", user_title),
        ParticipantUnmute(_) => format!("{} unmuted in call", user_title),
        ToggleGroupCallSetting(a) => format!("group call join muted: {}", a.join_muted),
        ParticipantJoinByInvite(_) => format!("{} joined by invite", user_title),
        ExportedInviteDelete(_) => "invite link deleted".to_string(),
        ExportedInviteRevoke(_) => "invite link revoked".to_string(),
        ExportedInviteEdit(_) => "invite link edited".to_string(),
        ParticipantVolume(_) => format!("{} volume changed in call", user_title),
        ChangeHistoryTtl(a) => format!("history TTL: {}s -> {}s", a.prev_value, a.new_value),
        ParticipantJoinByRequest(_) => format!("{} joined by request", user_title),
        ToggleNoForwards(a) => format!("no forwards: {}", if a.new_value { "enabled" } else { "disabled" }),
        SendMessage(a) => message_text(&a.message),
        ChangeAvailableReactions(_) => "available reactions changed".to_string(),
        ChangeUsernames(a) => format!("usernames: {:?} -> {:?}", a.prev_value, a.new_value),
        ToggleForum(a) => format!("forum: {}", if a.new_value { "enabled" } else { "disabled" }),
        CreateTopic(_) => "topic created".to_string(),
        EditTopic(_) => "topic edited".to_string(),
        DeleteTopic(_) => "topic deleted".to_string(),
        PinTopic(_) => "topic pinned/unpinned".to_string(),
        ToggleAntiSpam(a) => format!("anti-spam: {}", if a.new_value { "enabled" } else { "disabled" }),
        ChangePeerColor(_) => "peer color changed".to_string(),
        ChangeProfilePeerColor(_) => "profile peer color changed".to_string(),
        ChangeWallpaper(_) => "wallpaper changed".to_string(),
        ChangeEmojiStatus(_) => "emoji status changed".to_string(),
        ChangeEmojiStickerSet(_) => "emoji sticker set changed".to_string(),
        ToggleSignatureProfiles(a) => format!("signature profiles: {}", if a.new_value { "enabled" } else { "disabled" }),
        ParticipantSubExtend(_) => format!("{} subscription extended", user_title),
        ToggleAutotranslation(a) => format!("autotranslation: {}", if a.new_value { "enabled" } else { "disabled" }),
        ParticipantEditRank(a) => {
            let prev = if a.prev_rank.is_empty() { "none" } else { &a.prev_rank };
            let new = if a.new_rank.is_empty() { "none" } else { &a.new_rank };
            format!("{}: rank {} -> {}", user_title, prev, new)
        }
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
                    log_output: format_log_output(&ev.action, &user_title, &result.users),
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
