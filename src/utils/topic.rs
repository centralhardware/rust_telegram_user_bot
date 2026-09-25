use grammers_client::message::Message;
use grammers_client::Client;
use grammers_tl_types as tl;
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

/// Topic titles are stable and few per chat, so one lookup per topic is enough.
static TOPIC_NAMES: LazyLock<Mutex<HashMap<(i64, i32), String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The topic a message was posted in, ready for the `events_log` row: its id and
/// its title, or `(0, "")` outside a forum topic.
pub async fn topic_of(client: &Client, message: &Message) -> (i32, String) {
    match crate::utils::reply_target::topic_id(message) {
        Some(id) => (id, topic_name(client, message).await),
        None => (0, String::new()),
    }
}

/// The topic's title, or an empty string when the message is not in a topic or
/// the title cannot be fetched.
pub async fn topic_name(client: &Client, message: &Message) -> String {
    let topic_id = match crate::utils::reply_target::topic_id(message) {
        Some(id) => id,
        None => return String::new(),
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    if let Some(name) = TOPIC_NAMES.lock().await.get(&(chat_id, topic_id)) {
        return name.clone();
    }

    let name = fetch_topic_name(client, message, topic_id).await;

    // Only cache a real title: a failed fetch (offline, no access) must not
    // pin an empty name onto the topic for the rest of the process's life.
    if !name.is_empty() {
        TOPIC_NAMES.lock().await.insert((chat_id, topic_id), name.clone());
    }
    name
}

async fn fetch_topic_name(client: &Client, message: &Message, topic_id: i32) -> String {
    let peer_ref = match message.peer_ref().await {
        Ok(Some(peer)) => peer,
        _ => return String::new(),
    };

    let topics = client
        .invoke(&tl::functions::messages::GetForumTopicsById {
            peer: peer_ref.into(),
            topics: vec![topic_id],
        })
        .await;

    match topics {
        Ok(tl::enums::messages::ForumTopics::Topics(topics)) => topics
            .topics
            .iter()
            .find_map(|topic| match topic {
                tl::enums::ForumTopic::Topic(t) if t.id == topic_id => Some(t.title.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}
