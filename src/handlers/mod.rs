mod auto_cat;
mod deleted;
mod edited;
mod incoming;
mod outgoing;

pub use auto_cat::handle_auto_cat;
pub use deleted::save_deleted;
pub use edited::save_edited;
pub use incoming::save_incoming;
pub use outgoing::save_outgoing;

use grammers_client::update::Message;
use grammers_client::Client;
use grammers_tl_types as tl;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::LazyLock;
use tokio::sync::Mutex;

static TOPIC_NAMES: LazyLock<Mutex<HashMap<(i64, i32), String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn get_topic_id(message: &Message) -> Option<i32> {
    match &message.deref().raw {
        tl::enums::Message::Message(msg) => match &msg.reply_to {
            Some(tl::enums::MessageReplyHeader::Header(header)) => {
                if header.forum_topic {
                    header.reply_to_top_id.or(header.reply_to_msg_id)
                } else {
                    // outgoing messages may not have forum_topic flag,
                    // but reply_to_top_id still points to the topic
                    header.reply_to_top_id
                }
            }
            _ => None,
        },
        _ => None,
    }
}

pub(crate) async fn get_topic_name(
    client: &Client,
    message: &Message,
    topic_id: i32,
) -> String {
    let chat_id = message.peer_id().bare_id_unchecked();

    {
        let cache = TOPIC_NAMES.lock().await;
        if let Some(name) = cache.get(&(chat_id, topic_id)) {
            return name.clone();
        }
    }

    let name = fetch_topic_name(client, message, topic_id).await;

    let mut cache = TOPIC_NAMES.lock().await;
    cache.insert((chat_id, topic_id), name.clone());
    name
}

async fn fetch_topic_name(
    client: &Client,
    message: &Message,
    topic_id: i32,
) -> String {
    let peer_ref = match message.peer_ref().await {
        Some(p) => p,
        None => return topic_id.to_string(),
    };

    let input_peer: tl::enums::InputPeer = peer_ref.into();

    let result = client
        .invoke(&tl::functions::messages::GetForumTopicsById {
            peer: input_peer,
            topics: vec![topic_id],
        })
        .await;

    match result {
        Ok(tl::enums::messages::ForumTopics::Topics(topics)) => {
            for topic in &topics.topics {
                if let tl::enums::ForumTopic::Topic(t) = topic {
                    if t.id == topic_id {
                        return t.title.clone();
                    }
                }
            }
            topic_id.to_string()
        }
        Err(_) => topic_id.to_string(),
    }
}
