//! Backfilling a chat's history into `events_log`.
//!
//! The live log only knows what the account was listening for, so everything
//! sent before the bot existed — years, in the older chats — is simply absent.
//! This walks a chat's history through `messages.Search` and writes the rows the
//! updates never delivered, so the log reaches back as far as Telegram does.
//!
//! It walks the whole history and writes only what the log is missing. Starting
//! below the oldest stored id would be cheaper, but the log is not dense: one
//! reply backfilled in 2023 sits thousands of messages under everything else,
//! and a floor drawn there leaves the whole gap above it unfilled.
//!
//! Driven by `!backfill` typed into any chat, from this account:
//!
//! ```text
//! !backfill              — this chat, only the messages this account sent
//! !backfill all          — this chat, everyone's messages
//! !backfill <chat_id>    — that chat, only this account's messages
//! !backfill <chat_id> all
//! ```
//!
//! `<chat_id>` is the id as `events_log` stores it, and the `-100…` form
//! Telegram apps show is accepted too.

use grammers_client::Client;
use grammers_client::message::Message;
use grammers_session::Session;
use grammers_session::types::{PeerId, PeerRef};
use log::{info, warn};
use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::db::Event;

/// Rows per ClickHouse insert. A backfill is thousands of messages at once, and
/// a batch that size is a part `events_log` can take directly — the Buffer in
/// front of it is there to spare it the one-row inserts of live traffic, and
/// pushing a whole history through memory would be the one thing it is not for.
const BATCH: usize = 1_000;
/// How often the status message is rewritten, in messages seen.
const PROGRESS_EVERY: usize = 2_000;

/// Chats a backfill is running for. One at a time per chat: two walks of the
/// same history would only write each other's rows again.
static RUNNING: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Handle `!backfill` if this message is one. Returns whether it was.
pub async fn handle_command(client: &Client, message: &Message) -> bool {
    if !crate::utils::self_id::is_outgoing(message) {
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
    for arg in args.split_whitespace() {
        match arg {
            "all" => mine_only = false,
            "mine" => mine_only = true,
            other => match other.parse::<i64>() {
                Ok(id) => wanted_chat = Some(id),
                Err(_) => {
                    reply(message, &format!("backfill: don't understand `{other}`")).await;
                    return true;
                }
            },
        }
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
        Some(id) => match find_peer(normalize(id)).await {
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

    if crate::utils::log_ignore::is_log_ignored(chat_id) {
        reply(message, "backfill: that chat is in LOG_IGNORE_CHATS").await;
        return true;
    }

    {
        let mut running = RUNNING.lock().await;
        if !running.insert(chat_id) {
            reply(message, "backfill: already running for that chat").await;
            return true;
        }
    }

    let whose = if mine_only { "my messages" } else { "all messages" };
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

    let client = client.clone();
    tokio::spawn(async move {
        let outcome = run(&client, peer, chat_id, mine_only, status.as_ref()).await;
        if let Some(status) = &status {
            let _ = status.edit(outcome.as_str()).await;
        }
        info!("\x1b[96m{:<8} {:>8} {}\x1b[0m", "backfill", chat_id, outcome);
        RUNNING.lock().await.remove(&chat_id);
    });

    true
}

/// Telegram apps show a group as `-100…`; the log stores the bare id.
fn normalize(id: i64) -> i64 {
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
async fn find_peer(chat_id: i64) -> Option<PeerRef> {
    let session = crate::session::session()?;
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

/// Walk the chat's history newest-first, writing every message the log is
/// missing. Returns the line to leave in the status message.
async fn run(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    mine_only: bool,
    status: Option<&Message>,
) -> String {
    let mut search = client.search_messages(peer);
    if mine_only {
        search = search.sent_by_self();
    }

    let total = search.total().await.unwrap_or(0);
    let mut seen = 0usize;
    let mut written = 0usize;
    let mut batch: Vec<Event> = Vec::with_capacity(BATCH);
    let mut pending: Vec<Message> = Vec::with_capacity(BATCH);

    loop {
        let message = match search.next().await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(e) => {
                // Whatever is already in hand is still worth keeping.
                flush(&mut batch, &mut written).await;
                return format!(
                    "backfill {chat_id}: stopped after {seen} of {total} — {e}. \
                     Run it again to carry on."
                );
            }
        };
        seen += 1;
        pending.push(message);

        if pending.len() >= BATCH {
            convert(client, chat_id, &mut pending, &mut batch).await;
            flush(&mut batch, &mut written).await;
        }
        if seen % PROGRESS_EVERY == 0 {
            if let Some(status) = status {
                let _ = status
                    .edit(format!(
                        "backfill {chat_id}: {seen}/{total} read, {written} written…"
                    ))
                    .await;
            }
        }
    }

    convert(client, chat_id, &mut pending, &mut batch).await;
    flush(&mut batch, &mut written).await;

    format!("backfill {chat_id}: done — {seen} read, {written} written")
}

/// Turn the messages the log does not have yet into rows.
async fn convert(client: &Client, chat_id: i64, pending: &mut Vec<Message>, batch: &mut Vec<Event>) {
    if pending.is_empty() {
        return;
    }
    let known = known_ids(chat_id, &pending.iter().map(|m| m.id() as i64).collect::<Vec<_>>()).await;
    for message in pending.drain(..) {
        if known.contains(&(message.id() as i64)) {
            continue;
        }
        batch.push(crate::utils::event_of::event_of(client, &message).await);
    }
}

/// Which of these message ids the log already holds, asked a batch at a time.
/// Read through the Buffer, so a message logged a moment ago counts.
async fn known_ids(chat_id: i64, ids: &[i64]) -> HashSet<i64> {
    // The ids come from Telegram as integers, so the list is built rather than
    // bound: `clickhouse`'s `?` has no array form for an `IN`.
    let list = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    match crate::db::clickhouse()
        .query(&format!(
            "SELECT message_id FROM {} \
             WHERE chat_id = ? AND event IN (?, ?) AND NOT ephemeral \
             AND message_id IN ({list})",
            crate::db::EVENTS
        ))
        .bind(chat_id)
        .bind(crate::db::SEND)
        .bind(crate::db::SERVICE)
        .fetch_all::<i64>()
        .await
    {
        Ok(found) => found.into_iter().collect(),
        // Writing a row the log already has is harmless — `events_log` replaces
        // on merge — so a failed check is worth carrying on past.
        Err(e) => {
            warn!("backfill: checking existing ids: {e}");
            HashSet::new()
        }
    }
}

async fn flush(batch: &mut Vec<Event>, written: &mut usize) {
    if batch.is_empty() {
        return;
    }
    match crate::db::insert_rows("events_log", batch).await {
        Ok(()) => *written += batch.len(),
        Err(e) => warn!("backfill: insert: {e}"),
    }
    batch.clear();
}

async fn reply(message: &Message, text: &str) {
    if let Err(e) = message.reply(text).await {
        warn!("backfill: cannot reply: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

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
