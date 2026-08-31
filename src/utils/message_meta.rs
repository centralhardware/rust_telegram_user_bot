//! The parts of a message that are neither its text nor its media: where it was
//! forwarded from, which service action it announces, which album it belongs to,
//! and the flags Telegram sets on it.
//!
//! All of it is in `raw` already, but only as JSON. These are the fields worth
//! having as columns — the ones a query filters or groups by.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;

#[derive(Default)]
pub struct MessageMeta {
    pub fwd_from_user_id: u64,
    pub fwd_from_chat_id: i64,
    pub fwd_from_msg_id: i64,
    pub fwd_from_name: String,
    pub fwd_date: u32,
    pub action: String,
    pub grouped_id: u64,
    pub via_bot_id: u64,
    pub post_author: String,
    pub pinned: bool,
    pub silent: bool,
    pub noforwards: bool,
    pub ttl_period: u32,
}

pub fn of(message: &tl::enums::Message) -> MessageMeta {
    match message {
        tl::enums::Message::Message(msg) => of_message(msg),
        tl::enums::Message::Service(msg) => MessageMeta {
            action: crate::utils::service_action::kind(&msg.action),
            pinned: false,
            silent: msg.silent,
            ttl_period: msg.ttl_period.unwrap_or(0).max(0) as u32,
            ..MessageMeta::default()
        },
        tl::enums::Message::Empty(_) => MessageMeta::default(),
    }
}

fn of_message(msg: &tl::types::Message) -> MessageMeta {
    let mut meta = MessageMeta {
        grouped_id: msg.grouped_id.unwrap_or(0).max(0) as u64,
        via_bot_id: msg.via_bot_id.unwrap_or(0).max(0) as u64,
        post_author: msg.post_author.clone().unwrap_or_default(),
        pinned: msg.pinned,
        silent: msg.silent,
        noforwards: msg.noforwards,
        ttl_period: msg.ttl_period.unwrap_or(0).max(0) as u32,
        ..MessageMeta::default()
    };

    if let Some(tl::enums::MessageFwdHeader::Header(fwd)) = msg.fwd_from.as_ref() {
        meta.fwd_date = fwd.date.max(0) as u32;
        meta.fwd_from_name = fwd.from_name.clone().unwrap_or_default();
        // A channel post keeps its own id in the header, so a forward can be
        // traced back to the post it copies.
        meta.fwd_from_msg_id = fwd.channel_post.unwrap_or(0) as i64;
        if meta.post_author.is_empty() {
            meta.post_author = fwd.post_author.clone().unwrap_or_default();
        }
        match fwd.from_id.as_ref() {
            Some(peer @ tl::enums::Peer::User(_)) => {
                meta.fwd_from_user_id = PeerId::from(peer).bare_id_unchecked() as u64;
            }
            Some(peer) => {
                meta.fwd_from_chat_id = PeerId::from(peer).bot_api_dialog_id_unchecked();
            }
            None => {}
        }
    }

    meta
}
