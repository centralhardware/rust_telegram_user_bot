//! Reading Telegram's objects: what a message says, carries, answers and
//! announces, and turning a fetched message into its log row.

pub mod dialogs;
pub mod entities;
pub mod event_of;
pub mod media_description;
pub mod message_meta;
pub mod reply_target;
pub mod self_id;
pub mod service_action;
