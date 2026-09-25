//! `!backfill new`: the dialogs the log has never seen.

use super::*;

/// The key `RUNNING` holds while a `new` scan is on. A chat id is never 0, so
/// it can share the set with them and keep the one-at-a-time rule for free.
pub(super) const NEW_SCAN: i64 = 0;

/// Handle `!backfill new`: find the dialogs `events_log` has no row for and
/// walk each of them, one after another.
pub(super) async fn start_new(client: &Client, message: &Message, mine_only: bool, dry_run: bool) {
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
pub(super) async fn run_new(
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
            // A dialog `new` picked has no row in the log to start after.
            Start::Beginning,
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
pub(super) struct Dialog {
    pub(super) chat_id: i64,
    pub(super) peer: PeerRef,
    pub(super) title: String,
    /// Whether the chat is a conversation with a bot, off the flag the dialog
    /// list carried — the session's cache may never have been told.
    pub(super) bot: bool,
}

/// Every chat in the dialog list, read through the raw `messages.getDialogs`.
///
/// `Client::iter_dialogs` is not used here for the same reason `find_peer`
/// avoids it: it panics — "dialogs use an unknown peer" — on a dialog whose peer
/// the same response did not name, and `dialogCommunity` names none at all.
/// Nothing here needs a `Dialog` object: the id, the access hash and the title
/// are all on the `chats` and `users` of the response.
pub(super) async fn list_dialogs(client: &Client) -> Result<Scan, Box<dyn std::error::Error>> {
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
pub(super) struct Scan {
    /// The chats worth backfilling.
    pub(super) dialogs: Vec<Dialog>,
    /// Dialogs that name a peer the response described as unreadable — a chat
    /// this account was thrown out of, a `min` peer named only in passing.
    pub(super) unreadable: usize,
    /// Dialogs that name no peer at all: a community, which is a folder of
    /// chats rather than a chat.
    pub(super) peerless: usize,
}

/// The main dialog list, and the archive beside it.
pub(super) const MAIN_FOLDER: i32 = 0;
pub(super) const ARCHIVE_FOLDER: i32 = 1;

/// One folder's dialogs, added to what the other folders found. `seen` carries
/// across them: a chat is in one folder, but the pinned ones come with every
/// page of it.
pub(super) async fn folder_dialogs(
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
pub(super) fn address(
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
pub(super) fn describe(
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

pub(super) fn describe_chat(id: i64, chats: &[tl::enums::Chat]) -> Option<Dialog> {
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
pub(super) fn dialog_offset(dialog: &tl::enums::Dialog) -> Option<(tl::enums::Peer, i32)> {
    match dialog {
        tl::enums::Dialog::Dialog(d) => Some((d.peer.clone(), d.top_message)),
        tl::enums::Dialog::Folder(d) => Some((d.peer.clone(), d.top_message)),
        tl::enums::Dialog::Community(_) => None,
    }
}

/// When a message was sent, for the kinds that were sent at a time at all.
pub(super) fn message_date(message: &tl::enums::Message) -> Option<i32> {
    match message {
        tl::enums::Message::Message(m) => Some(m.date),
        tl::enums::Message::Service(m) => Some(m.date),
        tl::enums::Message::Empty(_) => None,
    }
}

/// Every chat id `events_log` holds a message for. Read through the Buffer, so a
/// chat logged a moment ago counts as seen.
pub(super) async fn logged_chat_ids() -> Result<HashSet<i64>, clickhouse::error::Error> {
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
