//! The set of chats the logged-in account administers, as last discovered by the
//! admin-log scheduler. Kept here so other parts of the bot can ask "do I run this
//! chat?" without re-walking the dialog list.

use std::collections::HashSet;
use std::sync::RwLock;

static ADMIN_CHATS: RwLock<Option<HashSet<u64>>> = RwLock::new(None);

pub fn set(ids: HashSet<u64>) {
    *ADMIN_CHATS.write().unwrap() = Some(ids);
}

/// False until the first discovery pass finishes, so nothing is archived from a
/// chat before we know we administer it.
pub fn contains(chat_id: u64) -> bool {
    ADMIN_CHATS
        .read()
        .unwrap()
        .as_ref()
        .is_some_and(|ids| ids.contains(&chat_id))
}
