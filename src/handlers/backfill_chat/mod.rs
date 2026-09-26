//! Backfilling a chat's history into `events_log`.
//!
//! The live log only knows what the account was listening for, so everything
//! sent before the bot existed — years, in the older chats — is simply absent.
//! This walks a chat's history through `messages.Search` and writes the rows the
//! updates never delivered, so the log reaches back as far as Telegram does.
//!
//! It writes only what the log is missing, but it reads the whole history to
//! find that out — the log's ids are full of holes that are not gaps, so they
//! cannot say which stretch was already walked. `last` picks up where an
//! earlier walk stopped, under the oldest message the log holds for the chat,
//! and `from <message_id>` starts under that one.
//!
//! Driven by `!backfill` typed into any chat, from this account:
//!
//! ```text
//! !backfill              — this chat, only the messages this account sent
//! !backfill all          — this chat, everyone's messages
//! !backfill <chat_id>    — that chat, only this account's messages
//! !backfill <chat_id> all
//! !backfill new          — every dialog the log has never seen, one after another
//! !backfill new all
//! !backfill new dry      — name what `new` would walk, and walk nothing
//! !backfill <chat_id> all last  — carry on down from the oldest message the
//!                                  log already holds for that chat
//! !backfill <chat_id> all from <message_id>  — walk down from that message
//! ```
//!
//! Without `last` a walk reads the chat's whole history and writes whatever the
//! log is missing; `last` is the cheap way to catch a chat up.
//!
//! `<chat_id>` is the id as `events_log` stores it, and the `-100…` form
//! Telegram apps show is accepted too.
//!
//! In a chat with a bot, a bare `/ping` and the `pong` it answers with are not
//! backfilled: they are the health check talking to itself, thousands of rows
//! saying only that both ends were up, and the log is no place for them.
//!
//! Every request to Telegram is followed by a short gap — a history read as
//! fast as Telegram will answer earns a FLOOD_WAIT of minutes, which is longer
//! than all the pauses together.
//!
//! A chat this account has left is walked like any other: left rather than
//! deleted, it is still in the dialog list and Telegram usually still answers
//! for its history. The ones it refuses are counted, not guessed at in advance.
//!
//! `new` reads the dialog list and backfills the chats `events_log` holds no row
//! for at all — the ones that existed before the bot did and have been silent
//! since. A chat with even one row in the log is left alone: it is the `<chat_id>`
//! form's job, which walks a history the log already reaches into.

use grammers_client::Client;
use grammers_client::message::Message;
use grammers_session::Session;
use grammers_session::types::{PeerId, PeerInfo, PeerRef};
use grammers_tl_types as tl;
use log::{debug, warn};
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::app::App;
use crate::utils::console::{LogLine, Tone};
use crate::db::Event;
use std::sync::Arc;

mod new;
mod walk;
use new::*;
use walk::*;

/// Rows per ClickHouse insert. A backfill is thousands of messages at once, and
/// a batch that size is a part `events_log` can take directly — the Buffer in
/// front of it is there to spare it the one-row inserts of live traffic, and
/// pushing a whole history through memory would be the one thing it is not for.
pub(super) const BATCH: usize = 1_000;
/// How many chats a dry run names in its message, before the rest is left to
/// the log. A Telegram message is 4096 characters and a report that does not fit
/// is not sent at all.
pub(super) const DRY_RUN_NAMES: usize = 50;
/// How often the status message is rewritten, in messages seen.
pub(super) const PROGRESS_EVERY: usize = 2_000;
/// How many messages are turned into rows at once. Building a row is mostly
/// waiting — on the reply target's lookup, on a name, now and then on Telegram
/// for a topic title — and done one after another that wait is the whole
/// backfill. Kept modest so the Telegram calls among them stay a trickle.
pub(super) const CONCURRENCY: usize = 16;
/// How long to wait between one Telegram request and the next. A backfill is
/// the one thing here that asks Telegram for years of history as fast as it
/// will answer, and the answer to that is a FLOOD_WAIT measured in minutes —
/// which costs more than every pause it would have taken to avoid it.
pub(super) const REQUEST_GAP: Duration = Duration::from_millis(500);
/// How many messages `messages.Search` answers with at once. Every this many
/// read is one round trip made, and one gap owed.
pub(super) const SEARCH_PAGE: usize = 100;

/// Chats a backfill is running for. One at a time per chat: two walks of the
/// same history would only write each other's rows again.
#[derive(Default)]
pub struct RunningBackfills(pub(super) Mutex<HashSet<i64>>);

/// Handle `!backfill` if this message is one. Returns whether it was.
pub async fn handle_command(app: &Arc<App>, message: &Message) -> bool {
    if !crate::utils::self_id::is_outgoing(app.me, message) {
        return false;
    }
    let text = message.text().trim();
    let Some(args) = text.strip_prefix("!backfill") else {
        return false;
    };
    // `!backfillsomething` is not the command.
    if !args.is_empty() && !args.starts_with(char::is_whitespace) {
        return false;
    }

    let mut mine_only = true;
    let mut wanted_chat: Option<i64> = None;
    let mut every_new = false;
    let mut start = Start::Newest;
    let mut dry_run = false;
    let mut words = args.split_whitespace();
    while let Some(arg) = words.next() {
        match arg {
            "all" => mine_only = false,
            "mine" => mine_only = true,
            "new" => every_new = true,
            "last" => start = Start::Last,
            "from" => match words.next().map(str::parse::<i64>) {
                Some(Ok(id)) if id > 0 => start = Start::From(id),
                _ => {
                    reply(message, "backfill: `from` needs a message id").await;
                    return true;
                }
            },
            "dry" => dry_run = true,
            other => match other.parse::<i64>() {
                Ok(id) => wanted_chat = Some(id),
                Err(_) => {
                    reply(message, &format!("backfill: don't understand `{other}`")).await;
                    return true;
                }
            },
        }
    }

    if dry_run && !every_new {
        reply(message, "backfill: `dry` is only for `new`").await;
        return true;
    }
    if every_new {
        if wanted_chat.is_some() {
            reply(message, "backfill: `new` takes no chat_id").await;
            return true;
        }
        if start != Start::Newest {
            reply(
                message,
                "backfill: `last` and `from` make no sense with `new` — \
                 a chat `new` picks has nothing logged",
            )
            .await;
            return true;
        }
        start_new(app, message, mine_only, dry_run).await;
        return true;
    }

    let here = message.peer_id().bare_id_unchecked();
    let (peer, chat_id) = match wanted_chat {
        None => match message.peer_ref().await {
            Ok(Some(peer)) => (peer, here),
            _ => {
                reply(message, "backfill: cannot resolve this chat").await;
                return true;
            }
        },
        Some(id) if normalize(id) == here => match message.peer_ref().await {
            Ok(Some(peer)) => (peer, here),
            _ => {
                reply(message, "backfill: cannot resolve this chat").await;
                return true;
            }
        },
        Some(id) => match find_peer(app, normalize(id)).await {
            Some(peer) => (peer, normalize(id)),
            None => {
                reply(
                    message,
                    &format!(
                        "backfill: chat_id {id} is not in the peer cache — \
                         say something there once and retry"
                    ),
                )
                .await;
                return true;
            }
        },
    };

    {
        let mut running = app.backfills.0.lock().await;
        if !running.insert(chat_id) {
            reply(message, "backfill: already running for that chat").await;
            return true;
        }
    }

    let whose = if mine_only {
        "my messages"
    } else {
        "all messages"
    };
    let status = match message
        .reply(format!("backfill {chat_id}: {whose}, starting…"))
        .await
    {
        Ok(status) => Some(status),
        Err(e) => {
            warn!("backfill: cannot post status: {e}");
            None
        }
    };

    let app = Arc::clone(app);
    tokio::spawn(async move {
        let bot_chat = is_bot_chat(&app, peer).await;
        let outcome = walk_chat(
            &app,
            peer,
            chat_id,
            mine_only,
            bot_chat,
            start,
            status.as_ref(),
        )
        .await;
        if let Some(status) = &status {
            let _ = status.edit(outcome.line.as_str()).await;
        }
        LogLine::new(Tone::Info, "backfill", chat_id).body(&outcome.to_string()).print();
        app.backfills.0.lock().await.remove(&chat_id);
    });

    true
}

/// Telegram apps show a group as `-100…`; the log stores the bare id.
pub(super) fn normalize(id: i64) -> i64 {
    let id = id.abs();
    match id.to_string().strip_prefix("100") {
        Some(rest) if id > 1_000_000_000_000 => rest.parse().unwrap_or(id),
        _ => id,
    }
}

/// The peer for a chat id, out of the session's peer cache: every peer the bot
/// has ever seen is stored there with the access hash Telegram needs back.
///
/// The cache is keyed by the id together with the kind of peer it is, and a
/// bare id says nothing about that — so all three are tried, and the one the
/// session knows is the answer. Listing dialogs would say it outright, but
/// grammers panics on a dialog whose peer the same response did not name, which
/// is a whole bot lost to a `!backfill` typo.
pub(super) async fn find_peer(app: &App, chat_id: i64) -> Option<PeerRef> {
    let session = app.session.as_ref()?;
    let candidates = [
        Some(PeerId::channel_unchecked(chat_id)),
        Some(PeerId::user_unchecked(chat_id)),
        PeerId::chat(chat_id),
    ];
    for id in candidates.into_iter().flatten() {
        match session.peer_ref(id).await {
            Ok(Some(peer)) => return Some(peer),
            Ok(None) => continue,
            Err(e) => {
                warn!("backfill: looking up peer {chat_id}: {e}");
                return None;
            }
        }
    }
    None
}

pub(super) async fn reply(message: &Message, text: &str) {
    if let Err(e) = message.reply(text).await {
        warn!("backfill: cannot reply: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::{is_health_check, normalize};

    #[test]
    fn the_health_check_is_the_command_and_its_answer() {
        assert!(is_health_check("/ping"));
        assert!(is_health_check("  /ping  "));
        assert!(is_health_check("/ping@some_bot"));
        assert!(is_health_check("pong"));
        assert!(is_health_check("Pong"));
    }

    #[test]
    fn anything_said_around_it_is_a_message_like_any_other() {
        assert!(!is_health_check("/ping the server for me"));
        assert!(!is_health_check("pong?"));
        assert!(!is_health_check("ping"));
        assert!(!is_health_check(""));
    }

    #[test]
    fn a_bare_id_is_left_alone() {
        assert_eq!(normalize(428985392), 428985392);
        assert_eq!(normalize(1234567890), 1234567890);
    }

    #[test]
    fn the_form_telegram_apps_show_becomes_the_one_the_log_stores() {
        assert_eq!(normalize(-1001234567890), 1234567890);
        assert_eq!(normalize(-428985392), 428985392);
    }
}
