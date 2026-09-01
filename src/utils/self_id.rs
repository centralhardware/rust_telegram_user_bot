//! Which messages this account wrote.
//!
//! Telegram leaves the `out` flag unset on the messages an account sends to its
//! own Saved Messages, so `Message::outgoing()` alone files them as incoming.
//! Comparing the sender against the account's own id is what actually decides
//! it: in a chat with itself the sender is the account, flag or no flag.

use grammers_client::message::Message;
use std::sync::OnceLock;

static SELF_ID: OnceLock<u64> = OnceLock::new();

/// Called once, as soon as `get_me` has answered.
pub fn set(id: u64) {
    let _ = SELF_ID.set(id);
}

pub fn is_outgoing(message: &Message) -> bool {
    if message.outgoing() {
        return true;
    }
    match message.sender_id() {
        // A sender with no bare id is grammers' "self" peer, which is the
        // account itself and so already the answer.
        Some(sender) => match sender.bare_id() {
            None => true,
            Some(id) => SELF_ID.get().is_some_and(|me| id == *me as i64),
        },
        None => false,
    }
}
