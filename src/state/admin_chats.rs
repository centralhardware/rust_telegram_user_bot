//! The set of chats the logged-in account administers, as last discovered by the
//! admin-log scheduler. Kept here so other parts of the bot can ask "do I run this
//! chat?" without re-walking the dialog list, and so the update handlers can flag
//! it stale when an update hints that our rights somewhere changed.

use std::collections::HashSet;
use std::sync::PoisonError;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Unknown (`None`) until the first discovery pass finishes, so nothing is
/// archived from a chat before we know we administer it.
#[derive(Default)]
pub struct AdminChats {
    ids: RwLock<Option<HashSet<u64>>>,
    stale: AtomicBool,
}

impl AdminChats {
    pub fn set(&self, ids: HashSet<u64>) {
        *self.ids.write().unwrap_or_else(PoisonError::into_inner) = Some(ids);
    }

    pub fn contains(&self, chat_id: u64) -> bool {
        self.ids
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|ids| ids.contains(&chat_id))
    }

    /// Ask for a fresh discovery pass. Several calls before the scheduler's next
    /// tick collapse into one walk of the dialog list.
    pub fn mark_stale(&self) {
        self.stale.store(true, Ordering::Relaxed);
    }

    /// Whether a rediscovery was asked for since the last call, clearing the flag.
    pub fn take_stale(&self) -> bool {
        self.stale.swap(false, Ordering::Relaxed)
    }
}
