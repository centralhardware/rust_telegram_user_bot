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
//! !backfill new          — every dialog the log has never seen, one after another
//! !backfill new all
//! ```
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
use log::{info, warn};
use std::collections::HashSet;
use std::time::Duration;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::db::Event;

/// Rows per ClickHouse insert. A backfill is thousands of messages at once, and
/// a batch that size is a part `events_log` can take directly — the Buffer in
/// front of it is there to spare it the one-row inserts of live traffic, and
/// pushing a whole history through memory would be the one thing it is not for.
const BATCH: usize = 1_000;
/// How often the status message is rewritten, in messages seen.
const PROGRESS_EVERY: usize = 2_000;
/// How many messages are turned into rows at once. Building a row is mostly
/// waiting — on the reply target's lookup, on a name, now and then on Telegram
/// for a topic title — and done one after another that wait is the whole
/// backfill. Kept modest so the Telegram calls among them stay a trickle.
const CONCURRENCY: usize = 16;
/// How long to wait between one Telegram request and the next. A backfill is
/// the one thing here that asks Telegram for years of history as fast as it
/// will answer, and the answer to that is a FLOOD_WAIT measured in minutes —
/// which costs more than every pause it would have taken to avoid it.
const REQUEST_GAP: Duration = Duration::from_millis(500);
/// How many messages `messages.Search` answers with at once. Every this many
/// read is one round trip made, and one gap owed.
const SEARCH_PAGE: usize = 100;

/// How many dialogs a page of `messages.getDialogs` asks for. Telegram's limit.
const DIALOG_PAGE: i32 = 100;

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
    let mut every_new = false;
    for arg in args.split_whitespace() {
        match arg {
            "all" => mine_only = false,
            "mine" => mine_only = true,
            "new" => every_new = true,
            other => match other.parse::<i64>() {
                Ok(id) => wanted_chat = Some(id),
                Err(_) => {
                    reply(message, &format!("backfill: don't understand `{other}`")).await;
                    return true;
                }
            },
        }
    }

    if every_new {
        if wanted_chat.is_some() {
            reply(message, "backfill: `new` takes no chat_id").await;
            return true;
        }
        start_new(client, message, mine_only).await;
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

    let client = client.clone();
    tokio::spawn(async move {
        let bot_chat = is_bot_chat(peer).await;
        let outcome = run(&client, peer, chat_id, mine_only, bot_chat, status.as_ref()).await;
        if let Some(status) = &status {
            let _ = status.edit(outcome.line.as_str()).await;
        }
        info!(
            "\x1b[96m{:<8} {:>8} {}\x1b[0m",
            "backfill", chat_id, outcome
        );
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

/// The key `RUNNING` holds while a `new` scan is on. A chat id is never 0, so
/// it can share the set with them and keep the one-at-a-time rule for free.
const NEW_SCAN: i64 = 0;

/// Handle `!backfill new`: find the dialogs `events_log` has no row for and
/// walk each of them, one after another.
async fn start_new(client: &Client, message: &Message, mine_only: bool) {
    {
        let mut running = RUNNING.lock().await;
        if !running.insert(NEW_SCAN) {
            reply(message, "backfill: a `new` scan is already running").await;
            return;
        }
    }

    let whose = if mine_only {
        "my messages"
    } else {
        "all messages"
    };
    let status = match message
        .reply(format!("backfill new: {whose}, reading the dialog list…"))
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
        let outcome = run_new(&client, mine_only, status.as_ref()).await;
        if let Some(status) = &status {
            let _ = status.edit(outcome.as_str()).await;
        }
        info!("\x1b[96m{:<8} {:>8} {}\x1b[0m", "backfill", "new", outcome);
        RUNNING.lock().await.remove(&NEW_SCAN);
    });
}

/// The body of a `new` scan. Returns the line to leave in the status message.
async fn run_new(client: &Client, mine_only: bool, status: Option<&Message>) -> String {
    let dialogs = match list_dialogs(client).await {
        Ok(dialogs) => dialogs,
        Err(e) => return format!("backfill new: cannot read the dialog list — {e}"),
    };
    let logged = match logged_chat_ids().await {
        Ok(ids) => ids,
        // Without the log's side of it every dialog would look new, and the
        // scan would walk the whole account's history for nothing.
        Err(e) => return format!("backfill new: cannot read the logged chats — {e}"),
    };

    let missing: Vec<Dialog> = dialogs
        .into_iter()
        .filter(|d| {
            !logged.contains(&d.chat_id) && !crate::utils::log_ignore::is_log_ignored(d.chat_id)
        })
        .collect();

    if missing.is_empty() {
        return "backfill new: nothing to do — every dialog is already in the log".to_string();
    }

    let total = missing.len();
    let mut done = 0usize;
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut refused = 0usize;
    for dialog in missing {
        // A chat the `<chat_id>` form is walking right now is left to it.
        if !RUNNING.lock().await.insert(dialog.chat_id) {
            skipped += 1;
            continue;
        }
        if let Some(status) = status {
            let _ = status
                .edit(format!(
                    "backfill new: {}/{total} — {} ({})…",
                    done + 1,
                    dialog.title,
                    dialog.chat_id
                ))
                .await;
        }
        let outcome = run(
            client,
            dialog.peer,
            dialog.chat_id,
            mine_only,
            dialog.bot,
            status,
        )
        .await;
        info!(
            "\x1b[96m{:<8} {:>8} {}\x1b[0m",
            "backfill", dialog.chat_id, outcome
        );
        written += outcome.written;
        refused += usize::from(outcome.refused);
        done += 1;
        RUNNING.lock().await.remove(&dialog.chat_id);
        tokio::time::sleep(REQUEST_GAP).await;
    }

    let busy = if skipped > 0 {
        format!(", {skipped} left to a backfill already running")
    } else {
        String::new()
    };
    let turned_away = if refused > 0 {
        format!(", {refused} Telegram would not answer for")
    } else {
        String::new()
    };
    format!(
        "backfill new: done — {done} of {total} chats walked, \
         {written} written{turned_away}{busy}"
    )
}

/// A dialog worth walking: the chat id as the log stores it, the peer to search
/// with, and a name for the status line.
struct Dialog {
    chat_id: i64,
    peer: PeerRef,
    title: String,
    /// Whether the chat is a conversation with a bot, off the flag the dialog
    /// list carried — the session's cache may never have been told.
    bot: bool,
}

/// Every chat in the dialog list, read through the raw `messages.getDialogs`.
///
/// `Client::iter_dialogs` is not used here for the same reason `find_peer`
/// avoids it: it panics — "dialogs use an unknown peer" — on a dialog whose peer
/// the same response did not name, and `dialogCommunity` names none at all.
/// Nothing here needs a `Dialog` object: the id, the access hash and the title
/// are all on the `chats` and `users` of the response.
async fn list_dialogs(client: &Client) -> Result<Vec<Dialog>, Box<dyn std::error::Error>> {
    let mut found = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    // The main list and the archive are separate folders, and a request names
    // one of them: asked for neither, Telegram answers with the main list and
    // the archive is simply missing — which is most of what the official
    // client's export finds and a scan of one folder does not.
    for folder_id in [MAIN_FOLDER, ARCHIVE_FOLDER] {
        folder_dialogs(client, folder_id, &mut seen, &mut found).await?;
    }
    Ok(found)
}

/// The main dialog list, and the archive beside it.
const MAIN_FOLDER: i32 = 0;
const ARCHIVE_FOLDER: i32 = 1;

/// One folder's dialogs, added to what the other folders found. `seen` carries
/// across them: a chat is in one folder, but the pinned ones come with every
/// page of it.
async fn folder_dialogs(
    client: &Client,
    folder_id: i32,
    seen: &mut HashSet<i64>,
    found: &mut Vec<Dialog>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut request = tl::functions::messages::GetDialogs {
        exclude_pinned: false,
        folder_id: Some(folder_id),
        offset_date: 0,
        offset_id: 0,
        offset_peer: tl::enums::InputPeer::Empty,
        limit: DIALOG_PAGE,
        hash: 0,
    };

    loop {
        use tl::enums::messages::Dialogs;
        let (dialogs, messages, chats, users, last_page) = match client.invoke(&request).await? {
            Dialogs::Dialogs(d) => (d.dialogs, d.messages, d.chats, d.users, true),
            Dialogs::Slice(d) => {
                let last = d.dialogs.len() < request.limit as usize;
                (d.dialogs, d.messages, d.chats, d.users, last)
            }
            // Only returned for a non-zero `hash`, which this never sends.
            Dialogs::NotModified(_) => break,
        };

        for dialog in &dialogs {
            let Some((peer, _)) = dialog_offset(dialog) else {
                continue;
            };
            let Some(dialog) = describe(&peer, &chats, &users) else {
                continue;
            };
            // The pinned dialogs come with the first page and again in place on
            // a later one.
            if !seen.insert(dialog.chat_id) {
                continue;
            }
            found.push(dialog);
        }

        if last_page {
            break;
        }

        // Where the next page starts: the last dialog of this one that can be
        // paged from at all. A community names no peer and holds no message, and
        // a peer the response did not describe cannot be addressed — and
        // `InputPeerEmpty` would page from the top again rather than skip ahead.
        //
        // Which dialog is a fit offset has nothing to do with which is worth
        // backfilling: a chat this account was thrown out of is skipped as a
        // chat and is still perfectly good as a place in the list. Ending the
        // scan on one — as this did — stopped it at the first banned chat that
        // happened to land last on a page, and lost every dialog under it.
        //
        // The date has to be the dialog's own. Telegram pages this list by date
        // above all, and a dialog whose top message the response did not carry
        // — deleted since, so the response holds a `messageEmpty` for it — has
        // none to offer. Carrying the last page's date over would ask for the
        // dialogs below a point the scan has already passed, and everything
        // between the two is never asked for at all: the old chats, the ones
        // whose last message is oldest. So a dialog that cannot say when it last
        // spoke is not the offset either, and the scan steps back to one that
        // can — at worst re-reading a dialog it has already seen, which the
        // `seen` set was there for.
        let Some((offset_id, offset_date, offset_peer)) =
            dialogs.iter().rev().find_map(|dialog| {
                let (peer, top_message) = dialog_offset(dialog)?;
                let peer = address(&peer, &chats, &users)?;
                let date = messages
                    .iter()
                    .find(|m| m.id() == top_message)
                    .and_then(message_date)?;
                Some((top_message, date, peer))
            })
        else {
            break;
        };
        // An offset that did not move would ask for the same page forever.
        if request.offset_id == offset_id && request.offset_date == offset_date {
            warn!("backfill: dialog paging stopped moving at message {offset_id}");
            break;
        }
        request.offset_id = offset_id;
        request.offset_date = offset_date;
        request.offset_peer = offset_peer;
        // The pinned dialogs came with the first page.
        request.exclude_pinned = true;
        tokio::time::sleep(REQUEST_GAP).await;
    }

    Ok(())
}

/// The `InputPeer` for a dialog's peer, out of the chats and users of the same
/// response — whatever kind of chat it is, and whether or not it is one worth
/// backfilling. `None` only for a peer the response did not describe.
fn address(
    peer: &tl::enums::Peer,
    chats: &[tl::enums::Chat],
    users: &[tl::enums::User],
) -> Option<tl::enums::InputPeer> {
    match peer {
        tl::enums::Peer::User(p) => users
            .iter()
            .find(|u| u.id() == p.user_id)
            .map(|u| PeerRef::from(u).into()),
        tl::enums::Peer::Chat(p) => chats
            .iter()
            .find(|c| c.id() == p.chat_id)
            .map(|c| PeerRef::from(c).into()),
        tl::enums::Peer::Channel(p) => chats
            .iter()
            .find(|c| c.id() == p.channel_id)
            .map(|c| PeerRef::from(c).into()),
    }
}

/// The chat id, peer and title for a dialog's peer, out of the chats and users
/// of the same response. `None` for a peer the response did not describe, or one
/// it described without the access hash needed to address it — a `min` user, a
/// chat the account was thrown out of. Neither can have its history searched.
fn describe(
    peer: &tl::enums::Peer,
    chats: &[tl::enums::Chat],
    users: &[tl::enums::User],
) -> Option<Dialog> {
    match peer {
        tl::enums::Peer::User(p) => {
            let user = users.iter().find(|u| u.id() == p.user_id)?;
            let tl::enums::User::User(user) = user else {
                return None;
            };
            if user.min || user.access_hash.is_none() {
                return None;
            }
            let title = match (&user.first_name, &user.last_name) {
                (Some(first), Some(last)) => format!("{first} {last}"),
                (Some(name), None) | (None, Some(name)) => name.clone(),
                (None, None) => user
                    .username
                    .clone()
                    .unwrap_or_else(|| format!("user {}", user.id)),
            };
            Some(Dialog {
                chat_id: user.id,
                peer: PeerRef::from(user),
                title,
                bot: user.bot,
            })
        }
        tl::enums::Peer::Chat(p) => describe_chat(p.chat_id, chats),
        tl::enums::Peer::Channel(p) => describe_chat(p.channel_id, chats),
    }
}

fn describe_chat(id: i64, chats: &[tl::enums::Chat]) -> Option<Dialog> {
    let chat = chats.iter().find(|c| c.id() == id)?;
    let title = match chat {
        // A group left rather than deleted is still in the dialog list and its
        // history is still readable — that is what the official client exports
        // when it exports a chat this account is no longer in. Left chats are
        // attempted, and the ones Telegram does refuse are counted and named
        // rather than guessed at from here.
        tl::enums::Chat::Chat(c) => c.title.clone(),
        tl::enums::Chat::Channel(c) => {
            // `min` describes a chat in passing, without an access hash.
            if c.min {
                return None;
            }
            c.title.clone()
        }
        // Empty, forbidden and community chats carry no history to search: a
        // chat this account was thrown out of answers nothing whatever it is
        // asked, so asking is a request spent on a certain refusal.
        _ => return None,
    };
    Some(Dialog {
        chat_id: id,
        peer: PeerRef::from(chat),
        title,
        bot: false,
    })
}

/// The peer and top message a dialog can be paged from, for the kinds that have
/// one. `dialogCommunity` has neither.
fn dialog_offset(dialog: &tl::enums::Dialog) -> Option<(tl::enums::Peer, i32)> {
    match dialog {
        tl::enums::Dialog::Dialog(d) => Some((d.peer.clone(), d.top_message)),
        tl::enums::Dialog::Folder(d) => Some((d.peer.clone(), d.top_message)),
        tl::enums::Dialog::Community(_) => None,
    }
}

/// When a message was sent, for the kinds that were sent at a time at all.
fn message_date(message: &tl::enums::Message) -> Option<i32> {
    match message {
        tl::enums::Message::Message(m) => Some(m.date),
        tl::enums::Message::Service(m) => Some(m.date),
        tl::enums::Message::Empty(_) => None,
    }
}

/// Every chat id `events_log` holds a message for. Read through the Buffer, so a
/// chat logged a moment ago counts as seen.
async fn logged_chat_ids() -> Result<HashSet<i64>, clickhouse::error::Error> {
    Ok(crate::db::clickhouse()
        .query(&format!(
            "SELECT DISTINCT chat_id FROM {} WHERE NOT ephemeral",
            crate::db::EVENTS
        ))
        .fetch_all::<i64>()
        .await?
        .into_iter()
        .collect())
}

/// Walk the chat's history newest-first, writing every message the log is
/// missing. Returns the line to leave in the status message.
async fn run(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    mine_only: bool,
    bot_chat: bool,
    status: Option<&Message>,
) -> Outcome {
    let mut search = client.search_messages(peer);
    if mine_only {
        search = search.sent_by_self();
    }

    let total = search.total().await.unwrap_or(0);
    let mut seen = 0usize;
    let mut written = 0usize;
    let mut pinged = 0usize;
    let mut batch: Vec<Event> = Vec::with_capacity(BATCH);
    let mut pending: Vec<Message> = Vec::with_capacity(BATCH);

    loop {
        let message = match search.next().await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(e) => {
                // Whatever is already in hand is still worth keeping.
                flush(&mut batch, &mut written).await;
                // Nothing read at all is Telegram turning the chat down rather
                // than a walk cut short: a left chat it will not answer for, a
                // chat this account was thrown out of since the list was read.
                let line = if seen == 0 {
                    format!("backfill {chat_id}: Telegram would not answer for it — {e}")
                } else {
                    format!(
                        "backfill {chat_id}: stopped after {seen} of {total} — {e}. \
                         Run it again to carry on."
                    )
                };
                return Outcome {
                    written,
                    refused: true,
                    line,
                };
            }
        };
        seen += 1;
        if bot_chat && is_health_check(message.text()) {
            pinged += 1;
            continue;
        }
        pending.push(message);

        if pending.len() >= BATCH {
            convert(client, chat_id, &mut pending, &mut batch).await;
            flush(&mut batch, &mut written).await;
        }
        // A page's worth read is a page's worth fetched: the next message asks
        // Telegram for the next one.
        if seen % SEARCH_PAGE == 0 {
            tokio::time::sleep(REQUEST_GAP).await;
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

    let health = if pinged > 0 {
        format!(", {pinged} health checks left out")
    } else {
        String::new()
    };
    Outcome {
        written,
        refused: false,
        line: format!("backfill {chat_id}: done — {seen} read, {written} written{health}"),
    }
}

/// A message that is nothing but the health check: the `/ping` sent to a bot,
/// or the `pong` it answers with. `/ping@thebot` is the same command addressed
/// the way a group needs it.
fn is_health_check(text: &str) -> bool {
    let text = text.trim();
    if text.eq_ignore_ascii_case("pong") {
        return true;
    }
    let command = text.split_once('@').map_or(text, |(command, _)| command);
    command.eq_ignore_ascii_case("/ping")
}

/// Whether a chat is the one-to-one conversation with a bot — the only place a
/// bare `/ping` is the health check rather than something someone said.
///
/// Read off the session's peer cache, which keeps the flag Telegram sent with
/// the user. A peer it does not know is taken as not a bot: leaving a real
/// message out of the log is the worse mistake of the two.
async fn is_bot_chat(peer: PeerRef) -> bool {
    let Some(session) = crate::session::session() else {
        return false;
    };
    match session.peer(peer.id).await {
        Ok(Some(PeerInfo::User { bot, .. })) => bot.unwrap_or(false),
        Ok(_) => false,
        Err(e) => {
            warn!("backfill: looking up whether {:?} is a bot: {e}", peer.id);
            false
        }
    }
}

/// What one chat's walk came to: the line to show for it, and how much of the
/// log it added — which a `new` scan adds up over every chat it walks.
struct Outcome {
    written: usize,
    /// Whether the walk ended on Telegram saying no — a left chat it will not
    /// hand the history of, most of the time.
    refused: bool,
    line: String,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

/// Turn the messages the log does not have yet into rows.
async fn convert(
    client: &Client,
    chat_id: i64,
    pending: &mut Vec<Message>,
    batch: &mut Vec<Event>,
) {
    if pending.is_empty() {
        return;
    }
    let known = known_ids(
        chat_id,
        &pending.iter().map(|m| m.id() as i64).collect::<Vec<_>>(),
    )
    .await;

    let mut building = JoinSet::new();
    for message in pending.drain(..) {
        if known.contains(&(message.id() as i64)) {
            continue;
        }
        let client = client.clone();
        building.spawn(async move { crate::utils::event_of::event_of(&client, &message).await });
        while building.len() >= CONCURRENCY {
            collect(&mut building, batch).await;
        }
    }
    while !building.is_empty() {
        collect(&mut building, batch).await;
    }
}

/// Take one finished row out of the set. A row that panicked on the way is lost
/// with a line in the log rather than taking the backfill down with it.
async fn collect(building: &mut JoinSet<Event>, batch: &mut Vec<Event>) {
    match building.join_next().await {
        Some(Ok(event)) => batch.push(event),
        Some(Err(e)) => warn!("backfill: building a row: {e}"),
        None => {}
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
