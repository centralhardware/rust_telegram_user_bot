use grammers_client::message::Message;
use grammers_client::peer::Peer;
use grammers_tl_types as tl;

pub struct SenderInfo {
    pub username: Vec<String>,
    pub first_name: String,
    pub second_name: String,
    pub user_id: u64,
}

pub struct ChatInfo {
    pub chat_title: String,
    pub chat_usernames: Vec<String>,
}

pub fn extract_community_tag(update: &tl::enums::Update) -> String {
    let msg = match update {
        tl::enums::Update::NewMessage(u) => &u.message,
        tl::enums::Update::NewChannelMessage(u) => &u.message,
        _ => return String::new(),
    };
    match msg {
        tl::enums::Message::Message(m) => m.from_rank.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn extract_sender(message: &Message) -> SenderInfo {
    match message.sender() {
        Some(Peer::User(user)) => SenderInfo {
            username: vec![user.username().unwrap_or_default().to_string()],
            first_name: user.first_name().unwrap_or_default().to_string(),
            second_name: user.last_name().unwrap_or_default().to_string(),
            user_id: user.id().bare_id_unchecked() as u64,
        },
        _ => SenderInfo {
            username: Vec::new(),
            first_name: String::new(),
            second_name: String::new(),
            user_id: 0,
        },
    }
}

pub fn extract_chat(message: &Message) -> ChatInfo {
    let (title, usernames) = match message.peer() {
        Some(Peer::Group(group)) => (
            group.title().unwrap_or_default().to_string(),
            group.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        Some(Peer::Channel(channel)) => (
            channel.title().to_string(),
            channel.usernames().into_iter().map(|s| s.to_string()).collect(),
        ),
        _ => (String::new(), Vec::new()),
    };

    let chat_title = if title.is_empty() {
        message
            .peer()
            .map(|p| p.name().unwrap_or_default().to_string())
            .unwrap_or_default()
    } else {
        title
    };

    ChatInfo {
        chat_title,
        chat_usernames: usernames,
    }
}
