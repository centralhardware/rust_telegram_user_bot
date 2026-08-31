use grammers_client::Client;
use grammers_client::peer::Peer;
use grammers_session::types::PeerRef;
use grammers_tl_types as tl;
use log::{error, info};
use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::db::AdminAction;

const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// How often the dialog list is re-scanned for chats where we are an admin.
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// A chat we administer, along with everything needed to poll and annotate its admin log.
struct AdminChat {
    peer: PeerRef,
    chat_id: u64,
    title: String,
    usernames: Vec<String>,
    admin_ids: HashSet<i64>,
}

pub fn start(client: Client, _client_id: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut chats: Vec<AdminChat> = Vec::new();
        let mut discovered_at: Option<Instant> = None;
        loop {
            interval.tick().await;

            if discovered_at.is_none_or(|at| at.elapsed() >= DISCOVERY_INTERVAL) {
                match discover_admin_chats(&client).await {
                    Ok(found) => {
                        // One chat per line: a comma-joined list of a dozen-odd
                        // titles is a single unreadable line in the log.
                        let titles = found
                            .iter()
                            .map(|c| format!("  {} ({})", c.title, c.chat_id))
                            .collect::<Vec<_>>()
                            .join("\n");
                        info!("admin log: watching {} chat(s):\n{}", found.len(), titles);
                        crate::utils::admin_chats::set(found.iter().map(|c| c.chat_id).collect());
                        chats = found;
                        discovered_at = Some(Instant::now());
                    }
                    Err(e) => error!("Failed to discover admin chats: {:?}", e),
                }
            }

            for chat in &chats {
                if let Err(e) = log_admin_actions(&client, chat).await {
                    error!("Failed to fetch admin actions for {}: {:?}", chat.title, e);
                }
            }
        }
    });
}

/// Every chat in the dialog list where the logged-in account holds admin rights.
///
/// Only channels and megagroups keep an admin log; basic groups are skipped.
async fn discover_admin_chats(
    client: &Client,
) -> Result<Vec<AdminChat>, Box<dyn std::error::Error>> {
    let mut chats = Vec::new();
    let mut dialogs = client.iter_dialogs();

    while let Some(dialog) = dialogs.next().await? {
        let peer = dialog.peer();
        // Monoforums (a channel's direct-messages chat) report admin rights but reject both
        // getAdminLog and getParticipants with CHANNEL_MONOFORUM_UNSUPPORTED.
        let is_admin = match peer {
            Peer::Channel(channel) => !channel.raw.monoforum && channel.admin_rights().is_some(),
            Peer::Group(group) => match &group.raw {
                tl::enums::Chat::Channel(c) => {
                    !c.monoforum && (c.creator || c.admin_rights.is_some())
                }
                _ => false,
            },
            _ => false,
        };
        if !is_admin {
            continue;
        }

        let Some(chat_id) = peer.id().bare_id() else {
            continue;
        };
        let peer_ref = match peer.to_ref().await {
            Ok(Some(r)) => r,
            Ok(None) => {
                error!("Cannot get peer ref for chat {}", chat_id);
                continue;
            }
            Err(e) => {
                error!("Cannot get peer ref for chat {}: {:?}", chat_id, e);
                continue;
            }
        };

        let mut usernames: Vec<String> = Vec::new();
        if let Some(u) = peer.username() {
            usernames.push(u.to_string());
        }
        for u in peer.usernames() {
            usernames.push(u.to_string());
        }

        let admin_ids = match fetch_admin_ids(client, peer_ref).await {
            Ok(ids) => ids,
            Err(e) => {
                error!("Cannot list admins of chat {}: {:?}", chat_id, e);
                HashSet::new()
            }
        };

        chats.push(AdminChat {
            peer: peer_ref,
            chat_id: chat_id as u64,
            title: peer.name().unwrap_or("unknown").to_string(),
            usernames,
            admin_ids,
        });
    }

    Ok(chats)
}

/// The user ids currently holding admin rights in a chat, used to flag the actor of each event.
async fn fetch_admin_ids(
    client: &Client,
    peer: PeerRef,
) -> Result<HashSet<i64>, Box<dyn std::error::Error>> {
    let channel: tl::enums::InputChannel = peer.into();
    let result = client
        .invoke(&tl::functions::channels::GetParticipants {
            channel,
            filter: tl::enums::ChannelParticipantsFilter::ChannelParticipantsAdmins,
            offset: 0,
            limit: 200,
            hash: 0,
        })
        .await?;

    Ok(match result {
        tl::enums::channels::ChannelParticipants::Participants(p) => p
            .participants
            .iter()
            .filter_map(participant_user_id)
            .collect(),
        tl::enums::channels::ChannelParticipants::NotModified => HashSet::new(),
    })
}

/// Stable name for an action, kept identical to the historical `{:?}`-derived values so the
/// `LowCardinality` dictionary stays consistent — but pinned here instead of tracking the
/// library's `Debug` impl.
fn action_type_name(action: &tl::enums::ChannelAdminLogEventAction) -> &'static str {
    use tl::enums::ChannelAdminLogEventAction::*;
    match action {
        ChangeTitle(_) => "ChangeTitle",
        ChangeAbout(_) => "ChangeAbout",
        ChangeUsername(_) => "ChangeUsername",
        ChangePhoto(_) => "ChangePhoto",
        ToggleInvites(_) => "ToggleInvites",
        ToggleSignatures(_) => "ToggleSignatures",
        UpdatePinned(_) => "UpdatePinned",
        EditMessage(_) => "EditMessage",
        DeleteMessage(_) => "DeleteMessage",
        ParticipantJoin => "ParticipantJoin",
        ParticipantLeave => "ParticipantLeave",
        ParticipantInvite(_) => "ParticipantInvite",
        ParticipantToggleBan(_) => "ParticipantToggleBan",
        ParticipantToggleAdmin(_) => "ParticipantToggleAdmin",
        ChangeStickerSet(_) => "ChangeStickerSet",
        TogglePreHistoryHidden(_) => "TogglePreHistoryHidden",
        DefaultBannedRights(_) => "DefaultBannedRights",
        StopPoll(_) => "StopPoll",
        ChangeLinkedChat(_) => "ChangeLinkedChat",
        ChangeLocation(_) => "ChangeLocation",
        ToggleSlowMode(_) => "ToggleSlowMode",
        StartGroupCall(_) => "StartGroupCall",
        DiscardGroupCall(_) => "DiscardGroupCall",
        ParticipantMute(_) => "ParticipantMute",
        ParticipantUnmute(_) => "ParticipantUnmute",
        ToggleGroupCallSetting(_) => "ToggleGroupCallSetting",
        ParticipantJoinByInvite(_) => "ParticipantJoinByInvite",
        ExportedInviteDelete(_) => "ExportedInviteDelete",
        ExportedInviteRevoke(_) => "ExportedInviteRevoke",
        ExportedInviteEdit(_) => "ExportedInviteEdit",
        ParticipantVolume(_) => "ParticipantVolume",
        ChangeHistoryTtl(_) => "ChangeHistoryTtl",
        ParticipantJoinByRequest(_) => "ParticipantJoinByRequest",
        ToggleNoForwards(_) => "ToggleNoForwards",
        SendMessage(_) => "SendMessage",
        ChangeAvailableReactions(_) => "ChangeAvailableReactions",
        ChangeUsernames(_) => "ChangeUsernames",
        ToggleForum(_) => "ToggleForum",
        CreateTopic(_) => "CreateTopic",
        EditTopic(_) => "EditTopic",
        DeleteTopic(_) => "DeleteTopic",
        PinTopic(_) => "PinTopic",
        ToggleAntiSpam(_) => "ToggleAntiSpam",
        ChangePeerColor(_) => "ChangePeerColor",
        ChangeProfilePeerColor(_) => "ChangeProfilePeerColor",
        ChangeWallpaper(_) => "ChangeWallpaper",
        ChangeEmojiStatus(_) => "ChangeEmojiStatus",
        ChangeEmojiStickerSet(_) => "ChangeEmojiStickerSet",
        ToggleSignatureProfiles(_) => "ToggleSignatureProfiles",
        ParticipantSubExtend(_) => "ParticipantSubExtend",
        ToggleAutotranslation(_) => "ToggleAutotranslation",
        ParticipantEditRank(_) => "ParticipantEditRank",
    }
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

fn group_call_participant_user_id(p: &tl::enums::GroupCallParticipant) -> Option<i64> {
    let tl::enums::GroupCallParticipant::Participant(p) = p;
    match &p.peer {
        tl::enums::Peer::User(u) => Some(u.user_id),
        _ => None,
    }
}

/// The user an action was performed *on* (banned, promoted, invited, muted), when there is one.
fn target_user_id(action: &tl::enums::ChannelAdminLogEventAction) -> Option<i64> {
    use tl::enums::ChannelAdminLogEventAction::*;
    match action {
        ParticipantInvite(a) => participant_user_id(&a.participant),
        ParticipantToggleBan(a) => participant_user_id(&a.new_participant),
        ParticipantToggleAdmin(a) => participant_user_id(&a.new_participant),
        ParticipantSubExtend(a) => participant_user_id(&a.new_participant),
        ParticipantEditRank(a) => Some(a.user_id),
        ParticipantMute(a) => group_call_participant_user_id(&a.participant),
        ParticipantUnmute(a) => group_call_participant_user_id(&a.participant),
        ParticipantVolume(a) => group_call_participant_user_id(&a.participant),
        _ => None,
    }
}

fn message_id(msg: &tl::enums::Message) -> i32 {
    match msg {
        tl::enums::Message::Empty(m) => m.id,
        tl::enums::Message::Message(m) => m.id,
        tl::enums::Message::Service(m) => m.id,
    }
}

fn forum_topic_id(topic: &tl::enums::ForumTopic) -> i32 {
    match topic {
        tl::enums::ForumTopic::Topic(t) => t.id,
        tl::enums::ForumTopic::Deleted(t) => t.id,
    }
}

/// The message an action was performed on, for the message-shaped actions.
fn action_message_id(action: &tl::enums::ChannelAdminLogEventAction) -> i32 {
    use tl::enums::ChannelAdminLogEventAction::*;
    match action {
        UpdatePinned(a) => message_id(&a.message),
        EditMessage(a) => message_id(&a.new_message),
        DeleteMessage(a) => message_id(&a.message),
        StopPoll(a) => message_id(&a.message),
        SendMessage(a) => message_id(&a.message),
        _ => 0,
    }
}

/// The forum topic an action was performed on, for the topic-shaped actions.
fn action_topic_id(action: &tl::enums::ChannelAdminLogEventAction) -> i32 {
    use tl::enums::ChannelAdminLogEventAction::*;
    match action {
        CreateTopic(a) => forum_topic_id(&a.topic),
        DeleteTopic(a) => forum_topic_id(&a.topic),
        EditTopic(a) => forum_topic_id(&a.new_topic),
        PinTopic(a) => a
            .new_topic
            .as_ref()
            .or(a.prev_topic.as_ref())
            .map(forum_topic_id)
            .unwrap_or(0),
        _ => 0,
    }
}

/// Before/after pair of an action, flattened to text.
///
/// Most of the admin log is a single value changing, so these two columns answer most questions
/// without touching the raw payload. Actions carrying a structure rather than a value (photos,
/// sticker sets, wallpapers, banned rights) leave them empty — the payload still has the detail.
fn action_values(action: &tl::enums::ChannelAdminLogEventAction) -> (String, String) {
    use tl::enums::ChannelAdminLogEventAction::*;
    let boolean = |v: bool| (String::new(), v.to_string());
    match action {
        ChangeTitle(a) => (a.prev_value.clone(), a.new_value.clone()),
        ChangeAbout(a) => (a.prev_value.clone(), a.new_value.clone()),
        ChangeUsername(a) => (a.prev_value.clone(), a.new_value.clone()),
        ChangeUsernames(a) => (a.prev_value.join(","), a.new_value.join(",")),
        ChangeLinkedChat(a) => (a.prev_value.to_string(), a.new_value.to_string()),
        ToggleSlowMode(a) => (a.prev_value.to_string(), a.new_value.to_string()),
        ChangeHistoryTtl(a) => (a.prev_value.to_string(), a.new_value.to_string()),
        ParticipantEditRank(a) => (a.prev_rank.clone(), a.new_rank.clone()),
        EditMessage(a) => (message_text(&a.prev_message), message_text(&a.new_message)),
        ToggleInvites(a) => boolean(a.new_value),
        ToggleSignatures(a) => boolean(a.new_value),
        TogglePreHistoryHidden(a) => boolean(a.new_value),
        ToggleNoForwards(a) => boolean(a.new_value),
        ToggleForum(a) => boolean(a.new_value),
        ToggleAntiSpam(a) => boolean(a.new_value),
        ToggleSignatureProfiles(a) => boolean(a.new_value),
        ToggleAutotranslation(a) => boolean(a.new_value),
        ToggleGroupCallSetting(a) => boolean(a.join_muted),
        _ => (String::new(), String::new()),
    }
}

/// Every restriction a `ChatBannedRights` can carry, paired with its name in the log line.
///
/// A `true` flag means the right is taken away, so the same list reads both ways: what was
/// restricted, and what was handed back.
const RESTRICTIONS: [(&str, fn(&tl::types::ChatBannedRights) -> bool); 23] = [
    ("view messages", |r| r.view_messages),
    ("send messages", |r| r.send_messages),
    ("send media", |r| r.send_media),
    ("send stickers", |r| r.send_stickers),
    ("send gifs", |r| r.send_gifs),
    ("send games", |r| r.send_games),
    ("send inline", |r| r.send_inline),
    ("embed links", |r| r.embed_links),
    ("send polls", |r| r.send_polls),
    ("change info", |r| r.change_info),
    ("invite users", |r| r.invite_users),
    ("pin messages", |r| r.pin_messages),
    ("manage topics", |r| r.manage_topics),
    ("send photos", |r| r.send_photos),
    ("send videos", |r| r.send_videos),
    ("send round videos", |r| r.send_roundvideos),
    ("send audios", |r| r.send_audios),
    ("send voices", |r| r.send_voices),
    ("send docs", |r| r.send_docs),
    ("send plain text", |r| r.send_plain),
    ("edit rank", |r| r.edit_rank),
    ("send reactions", |r| r.send_reactions),
    ("manage linked peers", |r| r.manage_linked_peers),
];

/// The restrictions a participant carries, or `None` when they have no ban record at all.
fn banned_rights(p: &tl::enums::ChannelParticipant) -> Option<&tl::types::ChatBannedRights> {
    match p {
        tl::enums::ChannelParticipant::Banned(b) => {
            let tl::enums::ChatBannedRights::Rights(r) = &b.banned_rights;
            Some(r)
        }
        _ => None,
    }
}

fn is_restricted(rights: Option<&tl::types::ChatBannedRights>, has: fn(&tl::types::ChatBannedRights) -> bool) -> bool {
    rights.is_some_and(has)
}

/// Names of the rights currently taken away.
fn restriction_names(rights: Option<&tl::types::ChatBannedRights>) -> Vec<&'static str> {
    RESTRICTIONS
        .iter()
        .filter(|(_, has)| is_restricted(rights, *has))
        .map(|(name, _)| *name)
        .collect()
}

/// A participant's standing in one word — what the arrow in the log line points from or to.
fn state(rights: Option<&tl::types::ChatBannedRights>) -> String {
    if is_restricted(rights, |r| r.view_messages) {
        return "banned".to_string();
    }
    let names = restriction_names(rights);
    if names.is_empty() {
        "member".to_string()
    } else {
        format!("restricted ({})", names.join(", "))
    }
}

/// " until <date>" for a timed restriction, empty when it never expires.
fn until_suffix(rights: Option<&tl::types::ChatBannedRights>) -> String {
    match rights.map(|r| r.until_date) {
        Some(ts) if ts != 0 => chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|dt| format!(" until {}", dt.format("%Y-%m-%d %H:%M UTC")))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// What a ban toggle actually did to the user's rights.
///
/// Telegram sends the whole before/after participant, so a full ban, a lift, and a single
/// permission being flipped all arrive as the same event — the difference only shows in the
/// rights themselves.
fn describe_ban_change(
    prev: &tl::enums::ChannelParticipant,
    new: &tl::enums::ChannelParticipant,
    name: &str,
) -> String {
    let prev_rights = banned_rights(prev);
    let new_rights = banned_rights(new);
    let was_banned = is_restricted(prev_rights, |r| r.view_messages);
    let is_banned = is_restricted(new_rights, |r| r.view_messages);
    let until = until_suffix(new_rights);

    // A full ban takes every right at once, so the individual flags say nothing worth logging.
    if is_banned {
        return format!("{}: {} -> banned{}", name, state(prev_rights), until);
    }

    if was_banned {
        return format!("{}: banned -> {}{}", name, state(new_rights), until);
    }

    let changed: Vec<(&str, bool)> = RESTRICTIONS
        .iter()
        .filter(|(_, has)| is_restricted(prev_rights, *has) != is_restricted(new_rights, *has))
        .map(|(right, has)| (*right, is_restricted(new_rights, *has)))
        .collect();

    if changed.is_empty() {
        return format!("{} restrictions unchanged", name);
    }

    // One right per line, names padded, so a long list can be skimmed down the arrows.
    let width = changed.iter().map(|(right, _)| right.len()).max().unwrap_or(0);
    let lines: Vec<String> = changed
        .iter()
        .map(|(right, restricted)| {
            let (prev, new) = if *restricted {
                ("allowed", "restricted")
            } else {
                ("restricted", "allowed")
            };
            format!("  {:width$}  {} -> {}", right, prev, new, width = width)
        })
        .collect();
    format!("{}:{}\n{}", name, until, lines.join("\n"))
}

fn participant_name(p: &tl::enums::ChannelParticipant, users: &[tl::enums::User]) -> String {
    participant_user_id(p)
        .map(|id| extract_user_info(users, id).0)
        .unwrap_or_default()
}

/// Human-readable one-liner for an event.
///
/// `colorize` adds ANSI escapes to the edit diff — only ever for the console; the stored copy
/// stays plain text.
fn format_log_output(
    action: &tl::enums::ChannelAdminLogEventAction,
    user_title: &str,
    users: &[tl::enums::User],
    colorize: bool,
) -> String {
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
            if colorize {
                crate::utils::diff::colorize_unified_diff(&diff, &prev, &new)
            } else {
                diff.trim_end().to_string()
            }
        }
        DeleteMessage(a) => message_text(&a.message),
        ParticipantJoin => format!("{} joined", user_title),
        ParticipantLeave => format!("{} left", user_title),
        ParticipantInvite(a) => format!("{} invited", participant_name(&a.participant, users)),
        ParticipantToggleBan(a) => describe_ban_change(
            &a.prev_participant,
            &a.new_participant,
            &participant_name(&a.new_participant, users),
        ),
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


async fn get_last_event_id(chat_id: u64) -> Result<u64, Box<dyn std::error::Error>> {
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
    chat: &AdminChat,
) -> Result<(), Box<dyn std::error::Error>> {
    let ch = crate::db::clickhouse();

    let min_id = get_last_event_id(chat.chat_id).await? as i64;
    let mut max_id: i64 = 0;
    let mut total_inserted: usize = 0;
    let mut new_last_id: u64 = 0;

    loop {
        let input_channel: tl::enums::InputChannel = chat.peer.into();

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
            let target_id = target_user_id(&ev.action);
            let (prev_value, new_value) = action_values(&ev.action);
            let console_output = format_log_output(&ev.action, &user_title, &result.users, true);
            let target_user_title = target_id
                .map(|id| extract_user_info(&result.users, id).0)
                .unwrap_or_default();

            let log = &AdminAction {
                date: ev.date as u32,
                event_id: ev.id as u64,
                chat_id: chat.chat_id,
                action_type: action_type_name(&ev.action).to_string(),
                user_id: ev.user_id as u64,
                message: action_message_json(&ev.action),
                log_output: format_log_output(&ev.action, &user_title, &result.users, false),
                usernames,
                chat_usernames: chat.usernames.clone(),
                chat_title: chat.title.clone(),
                user_title,
                message_id: action_message_id(&ev.action) as u32,
                topic_id: action_topic_id(&ev.action) as u32,
                prev_value,
                new_value,
                target_user_id: target_id.unwrap_or(0) as u64,
                target_user_title,
                user_is_admin: chat.admin_ids.contains(&ev.user_id),
            };

            info!(
                "admin    {:>12} {:<25} {:<20} {:<20}\n{}",
                log.event_id,
                &log.chat_title.chars().take(25).collect::<String>(),
                &log.action_type.chars().take(20).collect::<String>(),
                &log.user_title.chars().take(20).collect::<String>(),
                console_output,
            );

            insert.write(log).await?;
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
        info!(
            "[{}] Inserted {} entries. Last ID: {}",
            chat.title, total_inserted, new_last_id
        );
    }

    Ok(())
}
