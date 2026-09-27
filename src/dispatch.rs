//! Where an update goes.
//!
//! Each thing that happens to a new message, and each raw update the bot
//! understands, is a handler of its own; the lists below are the one place
//! that says which run and in what order.
//!
//! This is also where a handler's error ends up. Handlers return
//! `anyhow::Result` and leave logging to the caller: the pipeline here, the
//! media worker, or a scheduler's loop. The one exception is the database
//! layer. A failed lookup there answers "nothing found" and logs once, because
//! every caller would do exactly that anyway.

use std::future::Future;
use std::pin::Pin;

use grammers_client::session::types::PeerId;
use grammers_client::tl;
use grammers_client::update::{Message, Update};
use log::{error, warn};
use std::sync::Arc;

use crate::app::App;
use crate::db::Event;
use crate::handlers;
use sha2::Digest;

pub type Step<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Whether the rest of a pipeline still sees the message.
#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// One step of a pipeline over `M`, the state its steps share.
pub trait Handler<M>: Sync {
    fn handle<'a>(&'a self, msg: &'a mut M) -> Step<'a, Flow>;
}

/// Run `steps` in order until one of them says stop.
pub async fn run<M: Send>(steps: &[&dyn Handler<M>], msg: &mut M) {
    for step in steps {
        if step.handle(msg).await == Flow::Stop {
            return;
        }
    }
}

/// A new message on its way through [`NEW_MESSAGE`].
pub struct NewMessage {
    pub app: Arc<App>,
    pub message: Message,
    /// The row the message was logged as, once [`Save`] has run and succeeded.
    pub event: Option<Event>,
}

/// What happens to a new message, in order.
pub static NEW_MESSAGE: &[&dyn Handler<NewMessage>] = &[
    // First, so a reply — or a pin — finds the message it points at in the log.
    &BackfillReply,
    // A pin is not a message of its own: it is logged against the message it
    // pins, and nothing after this sees it.
    &Service,
    &Save,
    // After the save: the archiver writes the same row again once the file is
    // in S3, so it needs the row as it was logged.
    &Media,
    &AutoCat,
    // After the save: `!backfill` is a message like any other, and belongs in
    // the log with the rest.
    &BackfillCommand,
];

/// Raw updates, which grammers has no friendly variant for: ephemeral
/// messages, reactions, and what else can happen to a message after it is
/// sent — pinned or unpinned, voted in, or seen and forwarded often enough
/// for Telegram to say so. Each handler picks out the updates it knows.
/// A raw update and the app it arrived on.
pub struct RawUpdate<'a> {
    pub app: &'a App,
    pub update: tl::enums::Update,
}

pub static RAW: &[&dyn for<'a> Handler<RawUpdate<'a>>] =
    &[&Ephemeral, &Reactions, &Pins, &Polls, &Views];

pub async fn handle(app: &Arc<App>, update: Update) {
    if let Some(key) = shared_key(&update) {
        match sender_of(&update) {
            // One of the other accounts sent it, and logs it.
            Some(sender) if app.claims.sent_by_another(app.me, sender) => return,
            Some(sender) if sender == app.me => app.claims.take(app.me, key),
            // Another account got it first and writes it.
            _ if !app.claims.claim(app.me, key) => return,
            _ => {}
        }
    }
    match update {
        Update::NewMessage(message) => {
            let mut msg = NewMessage {
                app: Arc::clone(app),
                message,
                event: None,
            };
            run(NEW_MESSAGE, &mut msg).await;
        }
        Update::MessageEdited(message) => {
            if let Err(e) = handlers::save_edited(app, &message).await {
                error!("Failed to save edited message: {e:#}");
            }
        }
        Update::MessageDeleted(deletion) => {
            if let Err(e) = handlers::save_deleted(app, &deletion).await {
                error!("Failed to save deleted message: {e:#}");
            }
        }
        Update::Raw(raw) => {
            let mut raw = RawUpdate {
                app,
                update: raw.raw,
            };
            run(RAW, &mut raw).await
        }
        _ => {}
    }
}

/// Who sent a new or edited message, by user id.
fn sender_of(update: &Update) -> Option<u64> {
    match update {
        Update::NewMessage(m) | Update::MessageEdited(m) => m
            .sender_id()
            .and_then(|s| s.bare_id())
            .and_then(|id| u64::try_from(id).ok()),
        _ => None,
    }
}

/// What names this update among every account's copies of it, when every
/// account receives the same one: anything in a channel or supergroup. `None`
/// for a private chat or basic group, which each account logs as its own.
///
/// A raw update names its chat in a different field for every constructor, so
/// it is keyed by its bytes instead: two accounts' copies of one channel event
/// are the same bytes, and nothing that differs between accounts (a private
/// chat's peer, an account's own vote) can be. Updates with no peer at all to
/// tell them apart -- ephemeral ones -- are left to each account.
fn shared_key(update: &Update) -> Option<String> {
    use crate::db::ch::is_channel;
    use grammers_client::tl::Serializable;
    match update {
        Update::NewMessage(m) => {
            let chat = m.peer_id().bot_api_dialog_id_unchecked();
            is_channel(chat).then(|| format!("send:{chat}:{}", m.id()))
        }
        Update::MessageEdited(m) => {
            let chat = m.peer_id().bot_api_dialog_id_unchecked();
            is_channel(chat).then(|| {
                let at = m.edit_date().map(|d| d.as_second()).unwrap_or_default();
                format!("edit:{chat}:{}:{at}", m.id())
            })
        }
        Update::MessageDeleted(d) => d
            .channel_id()
            .map(|c| format!("delete:{c}:{:?}", d.messages())),
        Update::Raw(raw) => match &raw.raw {
            tl::enums::Update::ChannelMessageViews(_)
            | tl::enums::Update::ChannelMessageForwards(_)
            | tl::enums::Update::PinnedChannelMessages(_) => {
                let bytes = raw.raw.to_bytes();
                let hash: String = sha2::Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                Some(format!("raw:{hash}"))
            }
            // These carry the account's own reaction or vote beside the
            // counts, so the two copies differ; the counts are what is logged.
            tl::enums::Update::MessageReactions(u) => {
                let peer = PeerId::from(&u.peer);
                let tl::enums::MessageReactions::Reactions(r) = &u.reactions;
                let counts: Vec<_> = r
                    .results
                    .iter()
                    .map(|c| {
                        let tl::enums::ReactionCount::Count(c) = c;
                        (format!("{:?}", c.reaction), c.count)
                    })
                    .collect();
                is_channel(peer.bot_api_dialog_id_unchecked()).then(|| {
                    format!(
                        "reactions:{}:{}:{counts:?}",
                        peer.bot_api_dialog_id_unchecked(),
                        u.msg_id
                    )
                })
            }
            // A poll is one object for everyone who can see it, wherever it
            // was posted.
            tl::enums::Update::MessagePoll(u) => {
                let tl::enums::PollResults::Results(r) = &u.results;
                let voters: Vec<_> = r
                    .results
                    .iter()
                    .flatten()
                    .map(|a| {
                        let tl::enums::PollAnswerVoters::Voters(v) = a;
                        (v.option.clone(), v.voters)
                    })
                    .collect();
                Some(format!(
                    "poll:{}:{:?}:{voters:?}",
                    u.poll_id, r.total_voters
                ))
            }
            _ => None,
        },
        _ => None,
    }
}

struct BackfillReply;
impl Handler<NewMessage> for BackfillReply {
    fn handle<'a>(&'a self, m: &'a mut NewMessage) -> Step<'a, Flow> {
        Box::pin(async move {
            if let Err(e) = handlers::backfill_reply(&m.app, &m.message).await {
                warn!("Failed to backfill a reply: {e:#}");
            }
            Flow::Continue
        })
    }
}

struct Service;
impl Handler<NewMessage> for Service {
    fn handle<'a>(&'a self, m: &'a mut NewMessage) -> Step<'a, Flow> {
        Box::pin(async move {
            if handlers::save_service(&m.app, &m.message).await {
                Flow::Stop
            } else {
                Flow::Continue
            }
        })
    }
}

struct Save;
impl Handler<NewMessage> for Save {
    fn handle<'a>(&'a self, m: &'a mut NewMessage) -> Step<'a, Flow> {
        Box::pin(async move {
            let saved = if crate::telegram::self_id::is_outgoing(m.app.me, &m.message) {
                handlers::save_outgoing(&m.app, &m.message).await
            } else {
                handlers::save_incoming(&m.app, &m.message).await
            };
            match saved {
                Ok(event) => m.event = Some(event),
                Err(e) => error!("Failed to save message: {e:#}"),
            }
            Flow::Continue
        })
    }
}

struct Media;
impl Handler<NewMessage> for Media {
    fn handle<'a>(&'a self, m: &'a mut NewMessage) -> Step<'a, Flow> {
        Box::pin(async move {
            if let Some(event) = &m.event {
                handlers::save_media(&m.app, &m.message, event).await;
            }
            Flow::Continue
        })
    }
}

struct AutoCat;
impl Handler<NewMessage> for AutoCat {
    fn handle<'a>(&'a self, m: &'a mut NewMessage) -> Step<'a, Flow> {
        Box::pin(async move {
            if let Err(e) = handlers::handle_auto_cat(&m.message).await {
                error!("Failed to handle auto cat: {e:#}");
            }
            Flow::Continue
        })
    }
}

struct BackfillCommand;
impl Handler<NewMessage> for BackfillCommand {
    fn handle<'a>(&'a self, m: &'a mut NewMessage) -> Step<'a, Flow> {
        Box::pin(async move {
            handlers::backfill_command(&m.app, &m.message).await;
            Flow::Continue
        })
    }
}

struct Ephemeral;
impl<'r> Handler<RawUpdate<'r>> for Ephemeral {
    fn handle<'a>(&'a self, raw: &'a mut RawUpdate<'r>) -> Step<'a, Flow> {
        Box::pin(async move {
            let app = raw.app;
            let u = &mut raw.update;
            match u {
                tl::enums::Update::NewEphemeralMessage(u) => {
                    handlers::save_ephemeral(app, &u.message, "new").await
                }
                tl::enums::Update::EditEphemeralMessage(u) => {
                    handlers::save_ephemeral(app, &u.message, "edit").await
                }
                tl::enums::Update::DeleteEphemeralMessages(u) => {
                    handlers::save_ephemeral_deleted(app, &u.peer, &u.ids).await
                }
                _ => {}
            }
            Flow::Continue
        })
    }
}

struct Reactions;
impl<'r> Handler<RawUpdate<'r>> for Reactions {
    fn handle<'a>(&'a self, raw: &'a mut RawUpdate<'r>) -> Step<'a, Flow> {
        Box::pin(async move {
            let app = raw.app;
            let u = &mut raw.update;
            if let tl::enums::Update::MessageReactions(u) = u {
                handlers::save_reactions(app, u).await;
            }
            Flow::Continue
        })
    }
}

struct Pins;
impl<'r> Handler<RawUpdate<'r>> for Pins {
    fn handle<'a>(&'a self, raw: &'a mut RawUpdate<'r>) -> Step<'a, Flow> {
        Box::pin(async move {
            let app = raw.app;
            let u = &mut raw.update;
            let (peer, messages, pinned) = match u {
                tl::enums::Update::PinnedMessages(u) => {
                    (PeerId::from(&u.peer), &u.messages, u.pinned)
                }
                tl::enums::Update::PinnedChannelMessages(u) => (
                    PeerId::channel_unchecked(u.channel_id),
                    &u.messages,
                    u.pinned,
                ),
                _ => return Flow::Continue,
            };
            handlers::save_pinned(
                app,
                peer.bare_id_unchecked(),
                peer.bot_api_dialog_id_unchecked(),
                messages,
                pinned,
            )
            .await;
            Flow::Continue
        })
    }
}

struct Polls;
impl<'r> Handler<RawUpdate<'r>> for Polls {
    fn handle<'a>(&'a self, raw: &'a mut RawUpdate<'r>) -> Step<'a, Flow> {
        Box::pin(async move {
            let app = raw.app;
            let u = &mut raw.update;
            if let tl::enums::Update::MessagePoll(u) = u {
                handlers::save_poll(app, u).await;
            }
            Flow::Continue
        })
    }
}

struct Views;
impl<'r> Handler<RawUpdate<'r>> for Views {
    fn handle<'a>(&'a self, raw: &'a mut RawUpdate<'r>) -> Step<'a, Flow> {
        Box::pin(async move {
            let app = raw.app;
            let u = &mut raw.update;
            match u {
                tl::enums::Update::ChannelMessageViews(u) => {
                    handlers::save_views(app, u.channel_id, u.id, u.views.max(0) as u32, 0).await
                }
                tl::enums::Update::ChannelMessageForwards(u) => {
                    handlers::save_views(app, u.channel_id, u.id, 0, u.forwards.max(0) as u32).await
                }
                _ => {}
            }
            Flow::Continue
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Push(&'static str, Flow);
    impl Handler<Vec<&'static str>> for Push {
        fn handle<'a>(&'a self, seen: &'a mut Vec<&'static str>) -> Step<'a, Flow> {
            Box::pin(async move {
                seen.push(self.0);
                match self.1 {
                    Flow::Continue => Flow::Continue,
                    Flow::Stop => Flow::Stop,
                }
            })
        }
    }

    #[tokio::test]
    async fn steps_run_in_the_order_they_are_listed() {
        let mut seen = vec![];
        run(
            &[&Push("a", Flow::Continue), &Push("b", Flow::Continue)],
            &mut seen,
        )
        .await;
        assert_eq!(seen, ["a", "b"]);
    }

    #[tokio::test]
    async fn a_step_that_stops_hides_the_message_from_the_rest() {
        let mut seen = vec![];
        run(
            &[
                &Push("a", Flow::Continue),
                &Push("b", Flow::Stop),
                &Push("c", Flow::Continue),
            ],
            &mut seen,
        )
        .await;
        assert_eq!(seen, ["a", "b"]);
    }
}
