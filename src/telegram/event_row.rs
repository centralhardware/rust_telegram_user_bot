//! The send row for a message, built from the TL message and nothing else but
//! what had to be looked up for it.
//!
//! Every path that logs a message — one arriving live, one fetched for a
//! reply, one read back by a backfill — ends here, so there is one answer to
//! what a row holds. It is a plain function of its input, which is what lets
//! the tests below feed it recorded message shapes and check every column.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;

use crate::db::{Event, EventKind};
use crate::handlers::extract::ChatInfo;
use crate::telegram::reply_target::ReplyInfo;

/// What a row needs besides the message: the parts that take a lookup.
#[derive(Default)]
pub struct Context {
    pub chat: ChatInfo,
    /// The reply as `db::resolve_reply` settled it, and who sent its target.
    pub reply: ReplyInfo,
    pub reply_to_user_id: u64,
    pub topic_id: i32,
    pub topic_name: String,
    /// A service message's action, spelled out — it names the people it is
    /// about, which takes their names.
    pub action_desc: Option<String>,
    /// The `raw` column. A live message stores the update it came on, a
    /// fetched one the message itself, so the caller serialises it.
    pub raw: String,
}

/// The send row, with everything but its sender filled in: the caller sets
/// who sent it.
///
/// A service message — a join, a title change, a call — is a message like any
/// other: an id, a sender, a date and a place in the history. It is logged as
/// one, and `action` is what says it announces something rather than carrying
/// what someone wrote.
pub fn build(message: &tl::enums::Message, cx: Context) -> Event {
    let (id, date, peer) = match message {
        tl::enums::Message::Message(m) => (m.id, m.date, Some(&m.peer_id)),
        tl::enums::Message::Service(m) => (m.id, m.date, Some(&m.peer_id)),
        tl::enums::Message::Empty(m) => (m.id, 0, m.peer_id.as_ref()),
    };
    let chat_id = peer
        .map(|p| PeerId::from(p).bare_id_unchecked())
        .unwrap_or(0);

    // The text as the sender wrote it — its formatting and its buttons are
    // columns of their own — or, with no text, the action or the media it
    // carries.
    let text = crate::render::format_entities::plain_text_of(message);
    let content = if !text.is_empty() {
        text
    } else if let Some(action) = cx.action_desc {
        action
    } else {
        crate::telegram::media_description::describe_of(message).unwrap_or_default()
    };

    let meta = crate::telegram::media_description::media_meta_of(message).unwrap_or_default();
    let meta_msg = crate::telegram::message_meta::of(message);

    Event {
        date_time: date as u32,
        message: content,
        entities: crate::telegram::entities::of_raw(message),
        keyboard: crate::telegram::entities::keyboard_of_raw(message),
        chat_title: cx.chat.chat_title,
        chat_id,
        chat_usernames: cx.chat.chat_usernames,
        community_id: cx.chat.community_id,
        message_id: id as i64,
        reply_to: cx.reply.reply_to,
        reply_to_user_id: cx.reply_to_user_id,
        reply_to_chat_id: cx.reply.reply_to_chat_id,
        quote_text: cx.reply.quote_text,
        comment_to: cx.reply.comment_to,
        topic_id: cx.topic_id,
        topic_name: cx.topic_name,
        raw: cx.raw,
        media_type: meta.media_type,
        file_name: meta.file_name,
        mime_type: meta.mime_type,
        size: meta.size,
        duration: meta.duration,
        width: meta.width,
        height: meta.height,
        lat: meta.lat,
        lon: meta.lon,
        poll_question: meta.poll_question,
        poll_options: meta.poll_options,
        poll_id: meta.poll_id,
        fwd_from_user_id: meta_msg.fwd_from_user_id,
        fwd_from_chat_id: meta_msg.fwd_from_chat_id,
        fwd_from_msg_id: meta_msg.fwd_from_msg_id,
        fwd_from_name: meta_msg.fwd_from_name,
        fwd_date: meta_msg.fwd_date,
        action: meta_msg.action,
        grouped_id: meta_msg.grouped_id,
        via_bot_id: meta_msg.via_bot_id,
        guest_from_id: meta_msg.guest_from_id,
        sender_chat_id: meta_msg.sender_chat_id,
        post_author: meta_msg.post_author,
        pinned: meta_msg.pinned,
        silent: meta_msg.silent,
        noforwards: meta_msg.noforwards,
        ttl_period: meta_msg.ttl_period,
        ..Event::of(EventKind::Send)
    }
}

#[cfg(test)]
mod tests {
    //! Synthetic messages in the shape the `raw` column records them — every
    //! value made up — run through the builder the way the live path runs
    //! them: the reply read off the message, the lookups filled in by hand.

    use super::*;

    fn fixture(name: &str) -> tl::enums::Message {
        let json = match name {
            "text_reply" => include_str!("fixtures/text_reply.json"),
            "topic_message" => include_str!("fixtures/topic_message.json"),
            "channel_forward" => include_str!("fixtures/channel_forward.json"),
            "photo" => include_str!("fixtures/photo.json"),
            "service_title" => include_str!("fixtures/service_title.json"),
            _ => unreachable!(),
        };
        serde_json::from_str(json).unwrap()
    }

    /// What the live path hands the builder, minus anything looked up.
    fn row(name: &str, cx: Context) -> Event {
        let message = fixture(name);
        let reply = crate::telegram::reply_target::reply_info_of(&message);
        build(&message, Context { reply, ..cx })
    }

    fn chat() -> ChatInfo {
        ChatInfo {
            chat_title: "Chat".into(),
            chat_usernames: vec!["chat".into()],
            community_id: 0,
        }
    }

    #[test]
    fn a_text_reply_keeps_its_text_entities_and_target() {
        let e = row(
            "text_reply",
            Context {
                chat: chat(),
                reply_to_user_id: 9,
                ..Default::default()
            },
        );

        assert_eq!(e.event, EventKind::Send);
        // The bare id, the way every row keeps it — not the -100… dialog form.
        assert_eq!(e.chat_id, 1001);
        assert_eq!((e.message_id, e.date_time), (42, 1_700_000_000));
        assert_eq!(e.message, "hello world");
        assert_eq!(e.entities, [("bold".to_string(), 6, 5, String::new())]);
        assert_eq!(
            (e.reply_to, e.reply_to_user_id, e.reply_to_chat_id),
            (40, 9, 0)
        );
        assert_eq!(e.quote_text, "hi");
        assert_eq!(
            (e.chat_title.as_str(), e.chat_usernames.as_slice()),
            ("Chat", &["chat".to_string()][..])
        );
        assert!(e.media_type.is_empty() && e.action.is_empty());
    }

    #[test]
    fn a_plain_message_in_a_topic_is_not_a_reply_to_the_topic() {
        let e = row(
            "topic_message",
            Context {
                topic_id: 10,
                topic_name: "Topic".into(),
                ..Default::default()
            },
        );

        assert_eq!(e.reply_to, 0);
        assert_eq!((e.topic_id, e.topic_name.as_str()), (10, "Topic"));
    }

    #[test]
    fn a_forward_names_where_it_came_from() {
        let e = row("channel_forward", Context::default());

        assert_eq!(e.message, "forwarded text");
        assert_eq!(e.fwd_from_chat_id, 555);
        assert_eq!(e.fwd_from_msg_id, 77);
        assert_eq!(e.fwd_date, 1_690_000_000);
        assert_eq!(e.fwd_from_user_id, 0);
    }

    #[test]
    fn a_photo_without_a_caption_is_logged_as_its_description() {
        let e = row("photo", Context::default());

        assert_eq!(e.media_type, "photo");
        assert_eq!((e.width, e.height), (1280, 720));
        assert!(
            !e.message.is_empty(),
            "a captionless photo still says what it is"
        );
        assert!(e.entities.is_empty());
    }

    #[test]
    fn a_service_message_carries_its_action() {
        let e = row(
            "service_title",
            Context {
                action_desc: Some("[title changed to \"New title\"]".into()),
                ..Default::default()
            },
        );

        assert_eq!(e.chat_id, 1001);
        assert_eq!(e.message_id, 50);
        assert_eq!(e.message, "[title changed to \"New title\"]");
        assert!(!e.action.is_empty());
        assert!(e.media_type.is_empty());
    }

    #[test]
    fn the_raw_column_is_what_the_caller_serialised() {
        let e = row(
            "text_reply",
            Context {
                raw: "{\"NewMessage\":{}}".into(),
                ..Default::default()
            },
        );
        assert_eq!(e.raw, "{\"NewMessage\":{}}");
    }
}
