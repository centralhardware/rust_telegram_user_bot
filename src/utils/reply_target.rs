use grammers_client::message::Message;
use grammers_tl_types as tl;

/// The message this one actually replies to, or `None` when it replies to nothing.
///
/// In a forum topic Telegram points `reply_to_msg_id` at the topic-creation
/// message for *every* message in the topic, whether or not the sender replied
/// to anything — so `Message::reply_to_message_id()` alone reports the whole
/// topic as replies to its own header. Such a header either carries no
/// `reply_to_top_id` at all, or repeats the topic root in both fields (what the
/// Bot API produces when it posts with `message_thread_id`). A genuine reply
/// inside a topic keeps the topic root in `reply_to_top_id` and points
/// `reply_to_msg_id` at some *other* message.
pub fn reply_target(message: &Message) -> Option<i32> {
    let header = header(message)?;

    if header.forum_topic {
        match (header.reply_to_msg_id, header.reply_to_top_id) {
            (_, None) => return None,
            (Some(msg), Some(top)) if msg == top => return None,
            _ => {}
        }
    }

    header.reply_to_msg_id
}

/// The forum topic a message belongs to, or `None` outside a topic (a plain
/// chat, or the forum's General topic, which carries no reply header at all).
///
/// A genuine reply inside a topic keeps the topic root in `reply_to_top_id`;
/// every other message in the topic points `reply_to_msg_id` at it instead.
pub fn topic_id(message: &Message) -> Option<i32> {
    let header = header(message)?;

    if !header.forum_topic {
        return None;
    }

    header.reply_to_top_id.or(header.reply_to_msg_id)
}

fn header(message: &Message) -> Option<tl::types::MessageReplyHeader> {
    match message.reply_header() {
        Some(tl::enums::MessageReplyHeader::Header(header)) => Some(header),
        _ => None,
    }
}
