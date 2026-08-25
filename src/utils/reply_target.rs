use grammers_client::message::Message as FetchedMessage;
use grammers_client::update::Message;
use grammers_tl_types as tl;

/// The message this one actually replies to, or `None` when it replies to nothing.
///
/// In a forum topic Telegram points `reply_to_msg_id` at the topic-creation
/// message for *every* message in the topic, whether or not the sender replied
/// to anything — so `Message::reply_to_message_id()` alone reports the whole
/// topic as replies to its own header. A header like that carries `forum_topic`
/// with no `reply_to_top_id`; a genuine reply inside a topic keeps the topic
/// root in `reply_to_top_id` and the replied-to message in `reply_to_msg_id`.
pub fn reply_target(message: &Message) -> Option<i32> {
    from_header(message.reply_header())
}

/// Same rule for a fetched message, which is a different `Message` type with
/// the same reply header.
pub fn reply_target_fetched(message: &FetchedMessage) -> Option<i32> {
    from_header(message.reply_header())
}

fn from_header(header: Option<tl::enums::MessageReplyHeader>) -> Option<i32> {
    let header = match header {
        Some(tl::enums::MessageReplyHeader::Header(header)) => header,
        _ => return None,
    };

    if header.forum_topic && header.reply_to_top_id.is_none() {
        return None;
    }

    header.reply_to_msg_id
}
