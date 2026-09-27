//! The lock that lets only one account write an event every account sees.
//!
//! In a channel or supergroup every member sees the same message under the same
//! id, so with two accounts in one, each receives each update. The first
//! account to claim an update writes it; the other finds it claimed and drops
//! it -- the row, the console line, the media download and all. The accounts
//! run in one process, so the lock is a map in memory rather than anything in
//! ClickHouse, which has no way to insert only if absent.
//!
//! Private chats and basic groups never come through here: there each account
//! numbers its own messages, and the same id in two accounts is two messages.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a claim holds. Both accounts receive an update within seconds of
/// each other; this is long enough for a slow reconnect, short enough that the
/// map stays small.
const HOLD: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct Claims {
    held: Mutex<HashMap<String, (u64, Instant)>>,
    /// Every account running, by user id.
    accounts: Mutex<HashSet<u64>>,
}

impl Claims {
    /// An account is running and may claim.
    pub fn register(&self, account: u64) {
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account);
    }

    /// Whether `sender` is one of the running accounts other than `account`.
    /// A message one account sent is that account's to log: it is the one that
    /// sees it as outgoing, and the one a `!backfill` in it is meant for.
    pub fn sent_by_another(&self, account: u64, sender: u64) -> bool {
        sender != account
            && self
                .accounts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&sender)
    }

    /// `account` writes `key`, whoever claimed it: for a message it sent.
    pub fn take(&self, account: u64, key: String) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, (account, Instant::now()));
    }

    /// Whether `account` may write the event named by `key`: yes if it is the
    /// first to ask, or asked first before. Another account asking for the
    /// same key while the claim holds is told no.
    pub fn claim(&self, account: u64, key: String) -> bool {
        let now = Instant::now();
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.len() > 100_000 {
            held.retain(|_, (_, at)| now.duration_since(*at) < HOLD);
        }
        match held.get(&key) {
            Some((owner, at)) if now.duration_since(*at) < HOLD => *owner == account,
            _ => {
                held.insert(key, (account, now));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_account_writes_and_the_second_does_not() {
        let claims = Claims::default();
        assert!(claims.claim(1, "send:-1001:5".into()));
        assert!(!claims.claim(2, "send:-1001:5".into()));
        // The owner seeing it again (a redelivery) still writes.
        assert!(claims.claim(1, "send:-1001:5".into()));
        assert!(claims.claim(2, "send:-1001:6".into()));
    }

    #[test]
    fn a_message_an_account_sent_is_its_own() {
        let claims = Claims::default();
        claims.register(1);
        claims.register(2);
        assert!(claims.sent_by_another(1, 2));
        assert!(!claims.sent_by_another(1, 1));
        assert!(!claims.sent_by_another(1, 3));
    }
}
