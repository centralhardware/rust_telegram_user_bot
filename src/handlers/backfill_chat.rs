//! Backfilling a chat's history into `events_log`.
//!
//! The live log only knows what the account was listening for, so everything
//! sent before the bot existed — years, in the older chats — is simply absent.
//! This walks a chat's history through `messages.Search` and writes the rows the
//! updates never delivered, so the log reaches back as far as Telegram does.
//!
//! It writes only what the log is missing. Which stretch of the history it has
//! to read for that cannot be asked of `events_log`: its ids are full of holes
//! that are not gaps — a deleted message, the health checks a backfill leaves
//! out, everyone else's messages in a `mine` walk — and a floor drawn at the
//! oldest stored id is wrong for the same reason, one reply backfilled in 2023
//! sitting thousands of messages under everything else.
//!
//! So a walk records the contiguous id range it read in `backfill_state`, and
//! the next one reads only around it: from the newest message down to the top of
//! that range, and — if that walk never reached the start of the history — from
//! its bottom downwards. A chat walked to the end and quiet since costs two
//! requests instead of its whole history.
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
//! !backfill <chat_id> full  — ignore what is recorded as walked, read it all
//! !backfill <chat_id> mark  — record what the log holds as walked, without walking
//! !backfill <chat_id> mark partial  — the same, forced to count as unfinished
//! ```
//!
//! `full` is the way back if a recorded range is ever wrong: it reads the whole
//! history the way every backfill did before `backfill_state` existed, and
//! records the range again at the end. `mark` is the other direction — it takes
//! the range straight from the log for a chat that was walked before there was
//! anywhere to write that down, so that walk is not owed twice. It stops the
//! range where the stored ids stop being dense, so a walk that never finished is
//! marked only as far as it actually got, and the next backfill resumes under
//! there instead of reading it all again.
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
//! A supergroup made from a basic group is walked as two chats: the messages
//! said before the migration stayed in the old chat, under its own id, and that
//! chat is in no dialog list. Every backfill of a supergroup asks Telegram what
//! it was made from and walks that chat after it, on its own recorded range —
//! and writes the pair to `chat_migrations`, since `events_log` holds the two
//! halves as unrelated chats and nothing else says they are one conversation.
//!
//! `new` reads the dialog list and backfills the chats `events_log` holds no row
//! for at all — the ones that existed before the bot did and have been silent
//! since. A chat with even one row in the log is left alone: it is the `<chat_id>`
//! form's job, which walks a history the log already reaches into.

use clickhouse::Row;
use grammers_client::Client;
use grammers_client::message::Message;
use grammers_session::Session;
use grammers_session::types::{PeerId, PeerInfo, PeerKind, PeerRef};
use grammers_tl_types as tl;
use log::{debug, info, warn};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::db::Event;

/// Rows per ClickHouse insert. A backfill is thousands of messages at once, and
/// a batch that size is a part `events_log` can take directly — the Buffer in
/// front of it is there to spare it the one-row inserts of live traffic, and
/// pushing a whole history through memory would be the one thing it is not for.
const BATCH: usize = 1_000;
/// How many chats a dry run names in its message, before the rest is left to
/// the log. A Telegram message is 4096 characters and a report that does not fit
/// is not sent at all.
const DRY_RUN_NAMES: usize = 50;
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

/// The widest hole in a chat's stored ids that is still the ordinary kind: a
/// message deleted, a message the log never had a reason to keep. Chats run to
/// a couple of hundred missing ids in a row on that account alone. Anything
/// wider is where an unfinished walk stopped, and `mark` draws the line above
/// it rather than claiming the history under it.
const BIGGEST_ORDINARY_HOLE: i64 = 1_000;

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
    let mut full = false;
    let mut mark = false;
    let mut partial = false;
    let mut dry_run = false;
    for arg in args.split_whitespace() {
        match arg {
            "all" => mine_only = false,
            "mine" => mine_only = true,
            "new" => every_new = true,
            "full" => full = true,
            "mark" => mark = true,
            "partial" => partial = true,
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
        if full || mark {
            reply(
                message,
                "backfill: `full` and `mark` make no sense with `new` — \
                 a chat `new` picks has never been walked",
            )
            .await;
            return true;
        }
        start_new(client, message, mine_only, dry_run).await;
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

    if mark {
        if full {
            reply(message, "backfill: `mark` and `full` are opposites").await;
            return true;
        }
        reply(message, &mark_walked(chat_id, mine_only, !partial).await).await;
        return true;
    }
    if partial {
        reply(message, "backfill: `partial` only means something with `mark`").await;
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
        let outcome = run(
            &client,
            peer,
            chat_id,
            mine_only,
            bot_chat,
            full,
            status.as_ref(),
        )
        .await;
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
async fn start_new(client: &Client, message: &Message, mine_only: bool, dry_run: bool) {
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
        let outcome = run_new(&client, mine_only, dry_run, status.as_ref()).await;
        if let Some(status) = &status {
            let _ = status.edit(outcome.as_str()).await;
        }
        info!("\x1b[96m{:<8} {:>8} {}\x1b[0m", "backfill", "new", outcome);
        RUNNING.lock().await.remove(&NEW_SCAN);
    });
}

/// The body of a `new` scan. Returns the line to leave in the status message.
async fn run_new(
    client: &Client,
    mine_only: bool,
    dry_run: bool,
    status: Option<&Message>,
) -> String {
    let scan = match list_dialogs(client).await {
        Ok(scan) => scan,
        Err(e) => return format!("backfill new: cannot read the dialog list — {e}"),
    };
    let logged = match logged_chat_ids().await {
        Ok(ids) => ids,
        // Without the log's side of it every dialog would look new, and the
        // scan would walk the whole account's history for nothing.
        Err(e) => return format!("backfill new: cannot read the logged chats — {e}"),
    };

    let listed = scan.dialogs.len();
    let missing: Vec<Dialog> = scan
        .dialogs
        .into_iter()
        .filter(|d| {
            !logged.contains(&d.chat_id) && !crate::utils::log_ignore::is_log_ignored(d.chat_id)
        })
        .collect();

    // Where every dialog went, so a count that looks short can be read rather
    // than guessed at: what the list held, what the log already had, and what
    // Telegram named but will not answer for.
    let census = format!(
        "{listed} chats in the dialog list, {} already in the log, \
         {} unreadable, {} not a chat",
        listed - missing.len(),
        scan.unreadable,
        scan.peerless
    );
    info!("\x1b[96m{:<8} {:>8} {census}\x1b[0m", "backfill", "new");

    if missing.is_empty() {
        return format!("backfill new: nothing to do — {census}");
    }

    if dry_run {
        // What a real run would walk, named rather than counted, so it can be
        // held against the list the official client's export produces.
        let mut lines = vec![format!("backfill new (dry): {census}")];
        for dialog in missing.iter().take(DRY_RUN_NAMES) {
            lines.push(format!("{} — {}", dialog.chat_id, dialog.title));
        }
        // A message Telegram will not take is a report that never arrives; the
        // rest is in the log, which has no such limit.
        if missing.len() > DRY_RUN_NAMES {
            lines.push(format!(
                "…and {} more, all of them in the log",
                missing.len() - DRY_RUN_NAMES
            ));
        }
        for dialog in &missing {
            info!(
                "\x1b[96m{:<8} {:>8} would walk {}\x1b[0m",
                "backfill", dialog.chat_id, dialog.title
            );
        }
        return lines.join("\n");
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
            // A dialog `new` picked has no row in the log and so nothing
            // recorded as covered either.
            false,
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
         {written} written{turned_away}{busy}\n{census}"
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
async fn list_dialogs(client: &Client) -> Result<Scan, Box<dyn std::error::Error>> {
    let mut scan = Scan::default();
    let mut seen: HashSet<i64> = HashSet::new();
    // The main list and the archive are separate folders, and a request names
    // one of them: asked for neither, Telegram answers with the main list and
    // the archive is simply missing — which is most of what the official
    // client's export finds and a scan of one folder does not.
    for folder_id in [MAIN_FOLDER, ARCHIVE_FOLDER] {
        folder_dialogs(client, folder_id, &mut seen, &mut scan).await?;
    }
    Ok(scan)
}

/// What a walk of the dialog list came to. The counts are there to be reported:
/// a scan that finds fewer chats than expected is a question about where the
/// rest went, and the answer is one of these three numbers.
#[derive(Default)]
struct Scan {
    /// The chats worth backfilling.
    dialogs: Vec<Dialog>,
    /// Dialogs that name a peer the response described as unreadable — a chat
    /// this account was thrown out of, a `min` peer named only in passing.
    unreadable: usize,
    /// Dialogs that name no peer at all: a community, which is a folder of
    /// chats rather than a chat.
    peerless: usize,
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
    scan: &mut Scan,
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
                scan.peerless += 1;
                continue;
            };
            let Some(dialog) = describe(&peer, &chats, &users) else {
                scan.unreadable += 1;
                continue;
            };
            // The pinned dialogs come with the first page and again in place on
            // a later one.
            if !seen.insert(dialog.chat_id) {
                continue;
            }
            scan.dialogs.push(dialog);
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

/// Record what the log already holds for a chat as walked, without walking it:
/// `!backfill <chat_id> mark`.
///
/// For the chats backfilled before `backfill_state` existed. Their history is in
/// the log already, and reading a quarter of a million messages back out of
/// Telegram to find that out again is hours spent to write nothing.
///
/// It takes the log at its word, which is the one thing the walk itself never
/// does — so it does not take the whole of it: the range stops where the ids
/// stop being dense. Every chat's ids have holes, a deleted message here and
/// there, but a walk that was still working down through the history leaves one
/// hole orders of magnitude wider than those, with everything it never reached
/// under it. The mark is drawn above that hole and says it is not finished, and
/// the next backfill carries on below rather than treating it as the bottom.
///
/// The range is reported back to be looked at, and `!backfill <chat_id> full`
/// undoes it by reading everything again. `partial` forces the unfinished mark
/// for a log dense to the bottom that is still missing what lies under it.
async fn mark_walked(chat_id: i64, mine_only: bool, finished: bool) -> String {
    // The stored ids, and how far each one sits above the one below it. A hole
    // of a few dozen ids is the ordinary kind — a deleted message, a message the
    // log never had a reason to keep — and the walk that stopped part-way leaves
    // one enormously larger than those, which is the one worth finding.
    let bounds = crate::db::clickhouse()
        .query(&format!(
            "WITH ids AS ( \
                 SELECT DISTINCT message_id AS id FROM {} \
                 WHERE chat_id = ? AND event IN (?, ?) AND NOT ephemeral \
             ), \
             stepped AS ( \
                 SELECT id, id - lagInFrame(id, 1, id) OVER ( \
                     ORDER BY id ASC ROWS BETWEEN 1 PRECEDING AND CURRENT ROW \
                 ) AS step FROM ids \
             ) \
             SELECT min(id), max(id), count(), argMax(id, step), max(step) - 1 \
             FROM stepped",
            crate::db::EVENTS
        ))
        .bind(chat_id)
        .bind(crate::db::SEND)
        .bind(crate::db::SERVICE)
        .fetch_one::<(i64, i64, u64, i64, i64)>()
        .await;

    let (lowest, max_id, messages, above_hole, hole) = match bounds {
        Ok(bounds) => bounds,
        Err(e) => return format!("backfill {chat_id}: cannot read the log — {e}"),
    };
    if max_id == 0 {
        return format!("backfill {chat_id}: the log holds nothing for it — nothing to mark");
    }

    // Where the log stops being dense. A hole this wide is not a few messages
    // deleted: it is everything an unfinished walk never got to, so the mark
    // stops above it and the next backfill carries on from there.
    let broken = hole >= BIGGEST_ORDINARY_HOLE;
    let min_id = if broken { above_hole } else { lowest };
    let complete = finished && !broken;

    record(
        chat_id,
        mine_only,
        Covered {
            min_id,
            max_id,
            complete,
        },
        messages,
    )
    .await;

    let whose = if mine_only { "mine" } else { "all" };
    let found = if broken {
        format!(
            " — dense from {min_id} up, and a hole of {hole} ids under it, \
             so the next backfill carries on below {min_id}"
        )
    } else if complete {
        " — no hole worth the name in it, so the next backfill reads only above it".to_string()
    } else {
        format!(" — the next backfill carries on below {min_id}")
    };
    format!("backfill {chat_id}: marked {min_id}..{max_id} ({messages} rows, {whose}) as walked{found}. `full` to undo.")
}

/// The id range a finished walk has already read, out of `backfill_state`.
#[derive(Clone, Copy)]
struct Covered {
    min_id: i64,
    max_id: i64,
    /// Whether that walk reached the start of the history. When it did there is
    /// nothing under `min_id` to go back for.
    complete: bool,
}

/// What a chat's earlier walks covered, for the messages this one is after.
///
/// A `mine` walk reads the `all` row as well: everything an `all` walk stored
/// covers this account's messages inside the same range too. An `all` walk reads
/// only its own — a `mine` row says nothing about everyone else's messages.
async fn covered(chat_id: i64, mine_only: bool) -> Option<Covered> {
    match crate::db::clickhouse()
        .query(
            "SELECT min(min_id), max(max_id), min(complete) \
             FROM backfill_state FINAL \
             WHERE chat_id = ? AND (NOT mine_only OR ?) AND max_id > 0",
        )
        .bind(chat_id)
        .bind(mine_only)
        .fetch_one::<(i64, i64, bool)>()
        .await
    {
        Ok((_, 0, _)) => None,
        Ok((min_id, max_id, complete)) => Some(Covered {
            min_id,
            max_id,
            complete,
        }),
        // Not knowing what is covered costs a walk of the whole history, which
        // is what every backfill did before this table existed.
        Err(e) => {
            warn!("backfill: reading the covered range: {e}");
            None
        }
    }
}

#[derive(Row, Serialize)]
struct CoveredRow {
    chat_id: i64,
    mine_only: bool,
    min_id: i64,
    max_id: i64,
    complete: bool,
    messages: u64,
    walked_at: u32,
}

/// Write down what is covered now, so the next walk can jump over it.
async fn record(chat_id: i64, mine_only: bool, covered: Covered, messages: u64) {
    let row = CoveredRow {
        chat_id,
        mine_only,
        min_id: covered.min_id,
        max_id: covered.max_id,
        complete: covered.complete,
        messages,
        walked_at: crate::db::now(),
    };
    if let Err(e) = crate::db::insert_rows("backfill_state", &[row]).await {
        warn!("backfill: recording the covered range: {e}");
    }
}

/// What one segment of a walk read.
#[derive(Default)]
struct Read {
    seen: usize,
    pinged: usize,
    /// The ids at either end of what this segment actually read, 0 while it has
    /// read nothing.
    lowest: i64,
    highest: i64,
    /// Telegram's refusal, if the segment ended on one.
    error: Option<String>,
}

/// The highest id under `anchor` that still exists, out of `messages.getHistory`.
///
/// Only for the case the search cannot answer: an empty page where the walk has
/// read nothing yet. getHistory returns the next message that is really there
/// however wide the deleted stretch under the anchor is, and nothing at all when
/// the anchor is the start of the history.
///
/// It is not filtered to this account for a `mine` walk: what comes back is used
/// as a place to re-anchor the search, and the search does its own filtering.
async fn next_id_below(client: &Client, peer: PeerRef, anchor: i64) -> Option<i64> {
    let mut history = client.iter_messages(peer).offset_id(anchor as i32).limit(1);
    match history.next().await {
        Ok(Some(message)) => Some(message.id() as i64),
        Ok(None) => None,
        Err(e) => {
            warn!("backfill: reading the history under {anchor}: {e}");
            None
        }
    }
}

/// Walk one stretch of a chat's history, newest-first, writing every message the
/// log is missing.
///
/// Starts just under `offset_id` — 0 for the newest message there is — and stops
/// once it is past `floor`, which is the top of a range already covered. With
/// `floor` at 0 it runs to the start of the history.
///
/// A search that runs out of messages is re-anchored before that is believed:
/// Telegram answers a page with nothing in it when a wide stretch of the history
/// under the offset has been deleted, and taking that for the start of the
/// history seals the rest of it away — the range is recorded as complete and no
/// later walk ever goes back under it. Re-anchoring is at the lowest id read, or,
/// for a segment whose very first page came back empty, at whatever
/// `next_id_below` finds under the offset it started at.
#[allow(clippy::too_many_arguments)]
async fn walk(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    mine_only: bool,
    bot_chat: bool,
    offset_id: i64,
    floor: i64,
    total: usize,
    written: &mut usize,
    status: Option<&Message>,
) -> Read {
    let anchored = |at: i64| {
        let mut search = client.search_messages(peer);
        if mine_only {
            search = search.sent_by_self();
        }
        if at > 0 {
            search = search.offset_id(at as i32);
        }
        search
    };
    let mut search = anchored(offset_id);
    // The id the search was last re-anchored at, so an anchored search that ends
    // where it started is taken as the end rather than re-anchored for ever.
    let mut resumed_at = 0i64;

    let mut read = Read::default();
    let mut batch: Vec<Event> = Vec::with_capacity(BATCH);
    let mut pending: Vec<Message> = Vec::with_capacity(BATCH);

    loop {
        let message = match search.next().await {
            Ok(Some(message)) => message,
            // The search says it is out of messages, which is not the same as
            // the history being out of messages: `messages.Search` hands back an
            // empty page when a wide enough stretch under the offset is deleted,
            // and the iterator reports that as the end.
            //
            // Re-anchoring asks Telegram for what lies under that stretch. The
            // id to re-anchor at is the lowest one read, or — when the segment
            // read nothing at all, which is what an anchored segment starting on
            // top of such a stretch does — the id it was anchored at. That
            // second case cannot be re-anchored by the search alone: anchoring
            // again where it already was returns the same empty page. So the
            // next id that really exists under the anchor is asked of
            // `messages.getHistory`, which walks the history itself and has no
            // such quirk, and only an empty answer from it is the bottom.
            Ok(None) => {
                let anchor = if read.lowest > 0 {
                    read.lowest
                } else {
                    offset_id
                };
                if anchor == 0 || anchor == resumed_at {
                    break;
                }
                resumed_at = anchor;
                debug!("backfill {chat_id}: search ran out at {anchor}, looking under it");
                tokio::time::sleep(REQUEST_GAP).await;
                if read.lowest > 0 {
                    search = anchored(anchor);
                    continue;
                }
                match next_id_below(client, peer, anchor).await {
                    // `offset_id` is exclusive, so the search is anchored one
                    // above the id it must read first.
                    Some(next) if floor == 0 || next > floor => {
                        debug!(
                            "backfill {chat_id}: nothing under {anchor} in search, \
                             resuming at {next}"
                        );
                        search = anchored(next + 1);
                        continue;
                    }
                    _ => break,
                }
            }
            Err(e) => {
                read.error = Some(e.to_string());
                break;
            }
        };
        let id = message.id() as i64;
        // Everything from here down is already in the log, put there by the walk
        // that recorded the range. This is the whole saving: the segment ends
        // here instead of reading years of history to find nothing missing.
        if floor > 0 && id <= floor {
            break;
        }
        read.seen += 1;
        read.highest = read.highest.max(id);
        read.lowest = if read.lowest == 0 {
            id
        } else {
            read.lowest.min(id)
        };
        if bot_chat && is_health_check(message.text()) {
            read.pinged += 1;
            continue;
        }
        pending.push(message);

        if pending.len() >= BATCH {
            convert(client, chat_id, &mut pending, &mut batch).await;
            flush(&mut batch, written).await;
        }
        // A page's worth read is a page's worth fetched: the next message asks
        // Telegram for the next one.
        if read.seen % SEARCH_PAGE == 0 {
            tokio::time::sleep(REQUEST_GAP).await;
        }
        if read.seen % PROGRESS_EVERY == 0
            && let Some(status) = status {
                let _ = status
                    .edit(format!(
                        "backfill {chat_id}: {}/{total} read, {written} written…",
                        read.seen
                    ))
                    .await;
            }
    }

    convert(client, chat_id, &mut pending, &mut batch).await;
    // Whatever is already in hand is worth keeping even when the segment ended
    // on a refusal.
    flush(&mut batch, written).await;
    read
}

/// Walk a chat's history and, when it is a supergroup, the basic group it was
/// made from. Returns the line to show for the pair.
///
/// A supergroup made out of a basic group keeps none of that group's messages:
/// they stay where they were said, under the old chat's id, and the supergroup's
/// own history starts at the migration. The old chat is not in the dialog list
/// either — Telegram shows the pair as one chat — so nothing else here would
/// ever reach it, and every word said before the migration is out of the log for
/// good. `channelFull.migrated_from_chat_id` names it, and it is walked after the
/// supergroup, on its own id and with its own recorded range.
async fn run(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    mine_only: bool,
    bot_chat: bool,
    full: bool,
    status: Option<&Message>,
) -> Outcome {
    let mut outcome = walk_chat(client, peer, chat_id, mine_only, bot_chat, full, status).await;

    let Some(old_id) = migrated_from(client, peer, chat_id).await else {
        return outcome;
    };
    // Written down whether or not the walk below happens: `events_log` holds the
    // two as unrelated chats, and this row is the only thing that says the
    // history under `old_id` is the earlier half of this one.
    record_migration(chat_id, old_id).await;
    // A basic group needs no access hash: its bare id addresses it.
    let old_peer = PeerId::chat_unchecked(old_id).to_ambient_ref();

    if crate::utils::log_ignore::is_log_ignored(old_id) {
        return outcome;
    }
    // The chat the supergroup came from is a chat like any other, and a walk of
    // it may already be running under its own id.
    if !RUNNING.lock().await.insert(old_id) {
        outcome.line = format!(
            "{}\nbackfill {chat_id}: made from chat {old_id}, left to the backfill already running for it",
            outcome.line
        );
        return outcome;
    }
    if let Some(status) = status {
        let _ = status
            .edit(format!(
                "backfill {chat_id}: made from chat {old_id}, walking it too…"
            ))
            .await;
    }
    tokio::time::sleep(REQUEST_GAP).await;
    let before = walk_chat(client, old_peer, old_id, mine_only, false, full, status).await;
    RUNNING.lock().await.remove(&old_id);

    info!("\x1b[96m{:<8} {:>8} {}\x1b[0m", "backfill", old_id, before);
    Outcome {
        written: outcome.written + before.written,
        refused: outcome.refused || before.refused,
        line: format!(
            "{}\nbackfill {chat_id}: made from chat {old_id} — {}",
            outcome.line,
            before.line.trim_start_matches(&format!("backfill {old_id}: "))
        ),
    }
}

#[derive(Row, Serialize)]
struct MigrationRow {
    chat_id: i64,
    from_chat_id: i64,
    noticed_at: u32,
}

/// Write down that this supergroup was made from that chat.
///
/// Nothing else records it. The service message that says a supergroup was
/// created from a chat is only in the log for a migration this account was
/// listening through, and the old chats migrated years before the bot existed.
/// Without the pair, the history walked under the old id is a stranger's chat
/// sitting in the log next to the one it belongs to.
async fn record_migration(chat_id: i64, from_chat_id: i64) {
    let row = MigrationRow {
        chat_id,
        from_chat_id,
        noticed_at: crate::db::now(),
    };
    if let Err(e) = crate::db::insert_rows("chat_migrations", &[row]).await {
        warn!("backfill: recording the migration {from_chat_id} -> {chat_id}: {e}");
    }
}

/// The basic group a supergroup was made from, if it was made from one.
///
/// Only a channel can have been migrated from anything, and only `getFullChannel`
/// says so — the id is nowhere on the chat itself. A refusal is not an answer
/// worth stopping the backfill for: the supergroup's own history is walked either
/// way, and the pre-migration one is simply missed.
async fn migrated_from(client: &Client, peer: PeerRef, chat_id: i64) -> Option<i64> {
    if peer.id.kind() != PeerKind::Channel {
        return None;
    }
    if let Some(id) = full_channel_migrated_from(client, peer).await {
        return Some(id);
    }
    // Telegram does not always own up to it: `channelFull` left
    // `migrated_from_chat_id` out for a supergroup whose first message is the
    // migration itself. That message is the other witness, and the log has it
    // whenever the supergroup was walked at all — message 1, the service action
    // that says which chat it was made from.
    let from_log = migrated_from_log(chat_id).await;
    if let Some(id) = from_log {
        info!("backfill {chat_id}: made from chat {id}, off the log's own migration message");
    }
    from_log
}

/// What `channels.getFullChannel` says the supergroup was made from.
///
/// A refusal is not an answer worth stopping the backfill for: the supergroup's
/// own history is walked either way.
async fn full_channel_migrated_from(client: &Client, peer: PeerRef) -> Option<i64> {
    let channel: tl::enums::InputChannel = peer.into();
    let full = match client
        .invoke(&tl::functions::channels::GetFullChannel { channel })
        .await
    {
        Ok(tl::enums::messages::ChatFull::Full(full)) => full.full_chat,
        Err(e) => {
            warn!("backfill: asking what {:?} was made from: {e}", peer.id);
            return None;
        }
    };
    match full {
        tl::enums::ChatFull::ChannelFull(full) => full.migrated_from_chat_id,
        _ => None,
    }
}

/// The chat id out of the `channel_migrate_from` service message in the log.
///
/// The row's text is what `service_action::format` wrote for it — the title the
/// chat had, and its id at the end — so the id is read back off the end rather
/// than stored as a number anywhere. A row that does not end that way is left
/// alone: a wrong id here would send the walk at a stranger's chat.
async fn migrated_from_log(chat_id: i64) -> Option<i64> {
    let text: String = match crate::db::clickhouse()
        .query(&format!(
            "SELECT message FROM {} \
             WHERE chat_id = ? AND action = 'channel_migrate_from' AND NOT ephemeral \
             ORDER BY message_id ASC LIMIT 1",
            crate::db::EVENTS
        ))
        .bind(chat_id)
        .fetch_optional::<String>()
        .await
    {
        Ok(Some(text)) => text,
        Ok(None) => return None,
        Err(e) => {
            warn!("backfill {chat_id}: reading the migration message: {e}");
            return None;
        }
    };
    parse_migrated_from(&text)
}

/// The chat id at the end of a `channel_migrate_from` row:
/// `[supergroup created from chat "…", chat 175562287]`.
fn parse_migrated_from(text: &str) -> Option<i64> {
    let (_, tail) = text.trim_end_matches(']').rsplit_once(", chat ")?;
    tail.trim().parse::<i64>().ok().filter(|id| *id > 0)
}

/// Walk one chat's history, writing every message the log is missing, and skip
/// whatever an earlier walk already covered. Returns the line to show for it.
///
/// Two stretches at most: from the newest message down to the top of the covered
/// range, then — only if that earlier walk never reached the start of the history
/// — from its bottom downwards. `full` ignores the covered range and reads
/// everything, which is the way back if a recorded range is ever wrong.
#[allow(clippy::too_many_arguments)]
async fn walk_chat(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    mine_only: bool,
    bot_chat: bool,
    full: bool,
    status: Option<&Message>,
) -> Outcome {
    let known = if full {
        None
    } else {
        covered(chat_id, mine_only).await
    };

    let mut counter = client.search_messages(peer);
    if mine_only {
        counter = counter.sent_by_self();
    }
    let total = counter.total().await.unwrap_or(0);

    let mut written = 0usize;

    // Above the covered range: the messages that arrived since it was walked.
    let above = walk(
        client,
        peer,
        chat_id,
        mine_only,
        bot_chat,
        0,
        known.map_or(0, |c| c.max_id),
        total,
        &mut written,
        status,
    )
    .await;

    // Below it, when the earlier walk stopped short of the start of the history.
    let below = match known {
        Some(c) if !c.complete && above.error.is_none() => {
            tokio::time::sleep(REQUEST_GAP).await;
            walk(
                client,
                peer,
                chat_id,
                mine_only,
                bot_chat,
                c.min_id,
                0,
                total,
                &mut written,
                status,
            )
            .await
        }
        _ => Read::default(),
    };

    let seen = above.seen + below.seen;
    let pinged = above.pinged + below.pinged;
    let error = above.error.clone().or_else(|| below.error.clone());

    // What is covered now. A stretch that ended on a refusal still covers what
    // it read, as long as it joins onto the range already recorded — a walk cut
    // short above it leaves a hole in between, and a range claiming that hole
    // would hide it from every later walk.
    let now = match (known, above.highest, below.lowest) {
        (None, 0, _) => None,
        (None, high, _) => Some(Covered {
            min_id: above.lowest,
            max_id: high,
            complete: above.error.is_none(),
        }),
        (Some(c), high, low) => {
            let joins = high == 0 || above.lowest <= c.max_id + 1;
            let reached_bottom = below.seen > 0 && below.error.is_none();
            joins.then_some(Covered {
                min_id: if low > 0 { low.min(c.min_id) } else { c.min_id },
                max_id: c.max_id.max(high),
                complete: c.complete || reached_bottom,
            })
        }
    };
    if let Some(now) = now
        && seen > 0
    {
        record(chat_id, mine_only, now, seen as u64).await;
    }

    let health = if pinged > 0 {
        format!(", {pinged} health checks left out")
    } else {
        String::new()
    };
    // What the covered range spared this walk. Said as the range itself, not as
    // a count: `total` is every message Telegram has for the chat, and
    // `total - seen` would call the whole of it walked on the word of a range
    // that may cover a fraction.
    let jumped = match known {
        Some(c) => format!(", {}..{} already walked", c.min_id, c.max_id),
        None => String::new(),
    };

    if let Some(e) = error {
        // Nothing read at all is Telegram turning the chat down rather than a
        // walk cut short: a left chat it will not answer for, a chat this
        // account was thrown out of since the list was read.
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

    Outcome {
        written,
        refused: false,
        line: format!("backfill {chat_id}: done — {seen} read, {written} written{health}{jumped}"),
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
    use super::{is_health_check, normalize, parse_migrated_from};

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

    #[test]
    fn the_migration_message_names_the_chat_it_came_from() {
        assert_eq!(
            parse_migrated_from("[supergroup created from chat \"Космическая тр💥йка\", chat 175562287]"),
            Some(175562287)
        );
    }

    #[test]
    fn anything_else_names_nothing() {
        assert_eq!(parse_migrated_from("[migrated to supergroup 1149242811]"), None);
        assert_eq!(parse_migrated_from(""), None);
        assert_eq!(
            parse_migrated_from("[supergroup created from chat \"x\", chat nowhere]"),
            None
        );
    }
}
