use grammers_client::message::Message;
use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;

/// What a message replies to: the message id, the chat that id belongs to when
/// it is not this one, and the passage of it the sender quoted.
#[derive(Default)]
pub struct ReplyInfo {
    /// The message replied to, 0 when the message replies to nothing.
    pub reply_to: u64,
    /// The chat `reply_to` belongs to, 0 when it is the chat the reply itself
    /// was posted in — which is every ordinary reply.
    pub reply_to_chat_id: i64,
    /// The passage of the replied-to message the sender selected, empty when
    /// they quoted nothing and the whole message stands as the target.
    pub quote_text: String,
}

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

    if header.forum_topic && !is_foreign(&header) {
        match (header.reply_to_msg_id, header.reply_to_top_id) {
            (_, None) => return None,
            (Some(msg), Some(top)) if msg == top => return None,
            _ => {}
        }
    }

    header.reply_to_msg_id
}

/// `reply_target` together with the chat the target lives in and the quoted
/// passage.
///
/// Telegram lets a sender quote a message from a *different* chat: the reply
/// header then carries `reply_to_peer_id` naming that chat, and `reply_to_msg_id`
/// is an id in it, not in the chat the reply was posted in. Joining such an id
/// back onto this chat's messages finds nothing — or, worse, the unrelated
/// message that happens to carry the same id — so the chat it belongs to is kept
/// beside it. The quoted text is the only part of the target that is guaranteed
/// to be here at all: the source chat may be one the account never sees.
pub fn reply_info(message: &Message) -> ReplyInfo {
    let Some(reply_to) = reply_target(message) else {
        return ReplyInfo::default();
    };
    let Some(header) = header(message) else {
        return ReplyInfo::default();
    };

    ReplyInfo {
        reply_to: reply_to.max(0) as u64,
        reply_to_chat_id: foreign_chat_id(&header).unwrap_or(0),
        quote_text: header.quote_text.unwrap_or_default(),
    }
}

/// The forum topic a message belongs to, or `None` outside a topic (a plain
/// chat, or the forum's General topic, which carries no reply header at all).
///
/// A genuine reply inside a topic keeps the topic root in `reply_to_top_id`;
/// every other message in the topic points `reply_to_msg_id` at it instead. A
/// quote of another chat is the exception: its `reply_to_msg_id` is an id over
/// there, so it names no topic here and only `reply_to_top_id` can.
pub fn topic_id(message: &Message) -> Option<i32> {
    let header = header(message)?;

    if !header.forum_topic {
        return None;
    }

    if is_foreign(&header) {
        return header.reply_to_top_id;
    }

    header.reply_to_top_id.or(header.reply_to_msg_id)
}

/// Whether the header points at a message in another chat.
fn is_foreign(header: &tl::types::MessageReplyHeader) -> bool {
    header.reply_to_peer_id.is_some()
}

/// The chat the header points into, when that is not the chat the message was
/// posted in. Telegram fills `reply_to_peer_id` for exactly that case and leaves
/// it out of an ordinary same-chat reply.
///
/// Kept as a bare id, the way `chat_id` is: the point of the column is to be
/// joined against it, so the two have to be counted the same way. That is the
/// opposite convention from `fwd_from_chat_id`, which carries the Bot API sign.
fn foreign_chat_id(header: &tl::types::MessageReplyHeader) -> Option<i64> {
    header
        .reply_to_peer_id
        .as_ref()
        .map(|peer| PeerId::from(peer).bare_id_unchecked())
}

fn header(message: &Message) -> Option<tl::types::MessageReplyHeader> {
    match message.reply_header() {
        Some(tl::enums::MessageReplyHeader::Header(header)) => Some(header),
        _ => None,
    }
}
