//! What kind of thing an `events_log` row records, and the typed rows for the
//! kinds that fill only a few of its columns.
//!
//! The table stays one wide row: that is what the table wants. What goes into
//! it is built from a struct per kind, so a handler states exactly the fields
//! its kind has, and `From` fills in the rest of the row as empty. A message
//! send or edit fills most of the row and is still built as an `Event`
//! directly, from `Event::of(EventKind::Send)`.

use serde::{Serialize, Serializer};

use crate::db::Event;

/// The `event` column. Written, and bound into queries, as the same strings
/// the column has always held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventKind {
    #[default]
    Send,
    Edit,
    Delete,
    /// The counts as they stand after a change, a snapshot per update.
    Reaction,
    /// A service action performed on another message -- a pin. The row
    /// belongs to the message the action names, and carries the action rather
    /// than a text.
    Service,
    /// The archiver stored a message's file in S3. Its own row, naming the
    /// message it belongs to -- never the send row written a second time,
    /// which would be a duplicate the counters cannot tell from a real message.
    FileUploaded,
    /// A message pinned, and its undoing. A pin in a group is also announced
    /// as a service message; an unpin, and anything outside a group, is
    /// announced by nothing at all, so these rows are the only record of either.
    Pin,
    Unpin,
    /// A poll's results as they stand after a vote -- a snapshot, like a
    /// reaction.
    Poll,
    /// A channel post's view or forward counter, as it stands after the update.
    Views,
    /// A live location moving: one row per position Telegram reports, so the
    /// track can be read back in order. The message's first position is on
    /// its send row.
    Location,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Send => "send",
            EventKind::Edit => "edit",
            EventKind::Delete => "delete",
            EventKind::Reaction => "reaction",
            EventKind::Service => "service",
            EventKind::FileUploaded => "file_uploaded",
            EventKind::Pin => "pin",
            EventKind::Unpin => "unpin",
            EventKind::Poll => "poll",
            EventKind::Views => "views",
            EventKind::Location => "location",
        }
    }
}

/// As its string, both into a row and into a bound query parameter.
impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A message deleted. Telegram names nothing but the chat and the id, and that
/// is all the row keeps: what the message was is already on its send row.
pub struct DeleteEvent {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    /// Only an ephemeral deletion carries the title: its send row may be in
    /// no chat anyone else can read.
    pub chat_title: String,
    pub ephemeral: bool,
}

impl From<DeleteEvent> for Event {
    fn from(e: DeleteEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            chat_title: e.chat_title,
            ephemeral: e.ephemeral,
            ..Event::of(EventKind::Delete)
        }
    }
}

/// A message's reactions after a change.
pub struct ReactionEvent {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    pub reactions: Vec<(String, u32)>,
}

impl From<ReactionEvent> for Event {
    fn from(e: ReactionEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            reactions: e.reactions,
            ..Event::of(EventKind::Reaction)
        }
    }
}

/// A service action logged against the message it was performed on.
pub struct ServiceEvent {
    pub date_time: u32,
    pub chat_id: i64,
    /// The message the action was performed on.
    pub message_id: i64,
    /// The announcement's own id: the row is keyed on the message the action
    /// was performed on, so this is the only place it fits.
    pub service_message_id: i64,
    pub action: String,
}

impl From<ServiceEvent> for Event {
    fn from(e: ServiceEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            service_message_id: e.service_message_id,
            action: e.action,
            ..Event::of(EventKind::Service)
        }
    }
}

/// A message pinned or unpinned.
pub struct PinEvent {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    /// The state the message is in after the update: a pin when true, an unpin
    /// when not. Kept in `pinned` too, so a row read on its own says which way
    /// it went.
    pub pinned: bool,
}

impl From<PinEvent> for Event {
    fn from(e: PinEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            pinned: e.pinned,
            ..Event::of(if e.pinned {
                EventKind::Pin
            } else {
                EventKind::Unpin
            })
        }
    }
}

/// A poll's results after a vote.
pub struct PollEvent {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    pub topic_id: i32,
    pub poll_id: i64,
    pub poll_question: String,
    pub poll_options: Vec<String>,
    /// Voters per option, keyed by the option identifier.
    pub poll_results: Vec<(String, u32)>,
    pub poll_total_voters: u32,
}

impl From<PollEvent> for Event {
    fn from(e: PollEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            topic_id: e.topic_id,
            poll_id: e.poll_id,
            poll_question: e.poll_question,
            poll_options: e.poll_options,
            poll_results: e.poll_results,
            poll_total_voters: e.poll_total_voters,
            ..Event::of(EventKind::Poll)
        }
    }
}

/// A channel post's counters. Telegram reports each on its own, so the one
/// the update did not carry is 0.
pub struct ViewsEvent {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    pub views: u32,
    pub forwards: u32,
}

impl From<ViewsEvent> for Event {
    fn from(e: ViewsEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            views: e.views,
            forwards: e.forwards,
            ..Event::of(EventKind::Views)
        }
    }
}

/// A new position of a live location.
pub struct LocationEvent {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    pub user_id: u64,
    pub lat: f64,
    pub lon: f64,
}

impl From<LocationEvent> for Event {
    fn from(e: LocationEvent) -> Self {
        Event {
            date_time: e.date_time,
            chat_id: e.chat_id,
            message_id: e.message_id,
            user_id: e.user_id,
            lat: e.lat,
            lon: e.lon,
            media_type: "live_location".to_string(),
            ..Event::of(EventKind::Location)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_is_written_as_the_string_the_column_holds() {
        assert_eq!(
            serde_json::to_string(&EventKind::FileUploaded).unwrap(),
            "\"file_uploaded\""
        );
        assert_eq!(serde_json::to_string(&EventKind::Send).unwrap(), "\"send\"");
    }

    #[test]
    fn a_live_location_move_is_its_own_row() {
        let row: Event = LocationEvent {
            date_time: 1,
            chat_id: 2,
            message_id: 3,
            user_id: 4,
            lat: 18.8,
            lon: 98.97,
        }
        .into();
        assert_eq!(row.event, EventKind::Location);
        assert_eq!(serde_json::to_string(&row.event).unwrap(), "\"location\"");
        assert_eq!((row.lat, row.lon, row.user_id), (18.8, 98.97, 4));
        assert_eq!(row.media_type, "live_location");
        assert!(row.message.is_empty());
    }

    #[test]
    fn an_unpin_is_its_own_kind_and_says_so_in_pinned() {
        let row: Event = PinEvent {
            date_time: 1,
            chat_id: 2,
            message_id: 3,
            pinned: false,
        }
        .into();
        assert_eq!(row.event, EventKind::Unpin);
        assert!(!row.pinned);
    }

    #[test]
    fn a_typed_row_leaves_every_other_column_empty() {
        let row: Event = ViewsEvent {
            date_time: 1,
            chat_id: 2,
            message_id: 3,
            views: 10,
            forwards: 0,
        }
        .into();
        assert_eq!(row.event, EventKind::Views);
        assert_eq!(row.views, 10);
        assert!(row.message.is_empty() && row.raw.is_empty() && row.user_id == 0);
    }
}
