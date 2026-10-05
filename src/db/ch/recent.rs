//! The events this process has just written, so a replayed update is not
//! written twice.
//!
//! grammers sometimes hands over an update it already delivered, mostly when it
//! catches up on a channel after a gap. `events_log` does not mind: the copy has
//! the same sorting key and ReplacingMergeTree collapses it. The materialized
//! views behind the `v_*_stat` views do mind: they count every insert, so each
//! replay was counted twice there for good. Dropping a row whose key was
//! written moments ago keeps the aggregates equal to `events_log FINAL`.
//!
//! The key is the table's sorting key, so what is dropped here is exactly what
//! a merge would have dropped. Replays come within minutes, and only the last
//! [`CAPACITY`] keys are kept; a replay across a restart still gets through.

use std::collections::{HashSet, VecDeque};

use crate::db::Event;

const CAPACITY: usize = 20_000;

type Key = (i64, bool, i64, &'static str, u32, u64);

fn key(e: &Event) -> Key {
    (
        e.chat_id,
        e.ephemeral,
        e.message_id,
        e.event.as_str(),
        e.date_time,
        e.account_id,
    )
}

#[derive(Default)]
pub struct Recent {
    keys: HashSet<Key>,
    order: VecDeque<Key>,
}

impl Recent {
    /// The rows not written yet, now remembered as written. A key twice in
    /// one batch is kept once.
    pub fn fresh(&mut self, events: Vec<Event>) -> Vec<Event> {
        let mut rows = Vec::with_capacity(events.len());
        for e in events {
            let k = key(&e);
            if !self.keys.insert(k) {
                continue;
            }
            self.order.push_back(k);
            if self.order.len() > CAPACITY
                && let Some(old) = self.order.pop_front()
            {
                self.keys.remove(&old);
            }
            rows.push(e);
        }
        rows
    }

    /// Forget rows that were never written, so a retry of them is not dropped.
    pub fn forget(&mut self, events: &[Event]) {
        for e in events {
            self.keys.remove(&key(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::EventKind;

    fn send(message_id: i64) -> Event {
        Event {
            chat_id: 9,
            message_id,
            date_time: 100,
            ..Event::of(EventKind::Send)
        }
    }

    #[test]
    fn a_replay_is_dropped() {
        let mut recent = Recent::default();
        assert_eq!(recent.fresh(vec![send(1)]).len(), 1);
        assert!(recent.fresh(vec![send(1)]).is_empty());
    }

    #[test]
    fn a_later_edit_of_the_same_message_is_not_a_replay() {
        let mut recent = Recent::default();
        recent.fresh(vec![send(1)]);
        let edit = Event {
            date_time: 200,
            ..Event {
                event: EventKind::Edit,
                ..send(1)
            }
        };
        assert_eq!(recent.fresh(vec![edit]).len(), 1);
    }

    #[test]
    fn a_duplicate_within_one_batch_is_kept_once() {
        let mut recent = Recent::default();
        assert_eq!(recent.fresh(vec![send(1), send(1), send(2)]).len(), 2);
    }

    #[test]
    fn a_forgotten_row_can_be_written_again() {
        let mut recent = Recent::default();
        let rows = recent.fresh(vec![send(1)]);
        recent.forget(&rows);
        assert_eq!(recent.fresh(vec![send(1)]).len(), 1);
    }

    #[test]
    fn the_oldest_key_is_evicted_past_capacity() {
        let mut recent = Recent::default();
        recent.fresh((0..=CAPACITY as i64).map(send).collect());
        assert_eq!(recent.fresh(vec![send(0)]).len(), 1);
    }
}
