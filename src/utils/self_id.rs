//! Which messages this account wrote.
//!
//! Telegram leaves the `out` flag unset on the messages an account sends to its
//! own Saved Messages, so `Message::outgoing()` alone files them as incoming.
//! Comparing the sender against the account's own id is what actually decides
//! it: in a chat with itself the sender is the account, flag or no flag.

use grammers_client::message::Message;
/// `me` is the account's own id.
pub fn is_outgoing(me: u64, message: &Message) -> bool {
    if message.outgoing() {
        return true;
    }
    match message.sender_id() {
        // A sender with no bare id is grammers' "self" peer, which is the
        // account itself and so already the answer.
        Some(sender) => match sender.bare_id() {
            None => true,
            Some(id) => id == me as i64,
        },
        None => false,
    }
}
