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
    pub community_id: i64,
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
