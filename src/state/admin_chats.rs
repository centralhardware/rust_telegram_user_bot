//! The set of chats the logged-in account administers, as last discovered by the
//! admin-log scheduler. Kept here so other parts of the bot can ask "do I run this
//! chat?" without re-walking the dialog list.

use std::collections::HashSet;
use std::sync::PoisonError;
use std::sync::RwLock;

/// Unknown (`None`) until the first discovery pass finishes, so nothing is
/// archived from a chat before we know we administer it.
#[derive(Default)]
pub struct AdminChats(RwLock<Option<HashSet<u64>>>);

impl AdminChats {
    pub fn set(&self, ids: HashSet<u64>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(ids);
    }

    pub fn contains(&self, chat_id: u64) -> bool {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|ids| ids.contains(&chat_id))
    }
}
