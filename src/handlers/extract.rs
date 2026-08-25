use grammers_client::message::Message;
use grammers_client::peer::Peer;
use grammers_tl_types as tl;

#[derive(Clone, Default)]
pub struct SenderInfo {
    pub username: Vec<String>,
    pub first_name: String,
    pub second_name: String,
    pub user_id: u64,
}

#[derive(Clone, Default)]
pub struct ChatInfo {
    pub chat_title: String,
    pub chat_usernames: Vec<String>,
}

pub fn extract_community_tag_from_update(update: &tl::enums::Update) -> String {
    let msg = match update {
        tl::enums::Update::NewMessage(u) => &u.message,
        tl::enums::Update::NewChannelMessage(u) => &u.message,
        _ => return String::new(),
    };
    extract_community_tag(msg)
}

pub fn extract_community_tag(msg: &tl::enums::Message) -> String {
    match msg {
        tl::enums::Message::Message(m) => m.from_rank.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn extract_sender(message: &Message) -> SenderInfo {
    match message.sender() {
        Some(peer) => sender_from_peer(peer),
        None => SenderInfo::default(),
    }
}

/// The sender fields of an already-resolved peer. Only users send messages we
/// attribute; anything else stays blank, as it did before.
pub fn sender_from_peer(peer: &Peer) -> SenderInfo {
    match peer {
        Peer::User(user) => SenderInfo {
            username: vec![user.username().unwrap_or_default().to_string()],
            first_name: user.first_name().unwrap_or_default().to_string(),
            second_name: user.last_name().unwrap_or_default().to_string(),
            user_id: user.id().bare_id_unchecked() as u64,
        },
        _ => SenderInfo::default(),
    }
}

pub fn extract_chat(message: &Message) -> ChatInfo {
    match message.peer() {
        Some(peer) => chat_from_peer(peer),
        None => ChatInfo::default(),
    }
}

/// The chat fields of an already-resolved peer. For a private conversation the
/// "chat" is the other person, so their full name is the title.
pub fn chat_from_peer(peer: &Peer) -> ChatInfo {
    let chat_title = match peer {
        Peer::User(user) => {
            let first = user.first_name().unwrap_or_default();
            let last = user.last_name().unwrap_or_default();
            if last.is_empty() {
                first.to_string()
            } else {
                format!("{first} {last}")
            }
        }
        _ => peer.name().unwrap_or_default().to_string(),
    };

    ChatInfo {
        chat_title,
        chat_usernames: peer.usernames().into_iter().map(|s| s.to_string()).collect(),
    }
}
