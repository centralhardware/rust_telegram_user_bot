//! `!backfill new`: the dialogs the log has never seen.

use super::*;
use crate::utils::dialogs::{dialog_offset, Pages, ARCHIVE_FOLDER, MAIN_FOLDER};

/// The key `RUNNING` holds while a `new` scan is on. A chat id is never 0, so
/// it can share the set with them and keep the one-at-a-time rule for free.
pub(super) const NEW_SCAN: i64 = 0;

/// Handle `!backfill new`: find the dialogs `events_log` has no row for and
/// walk each of them, one after another.
pub(super) async fn start_new(app: &Arc<App>, message: &Message, mine_only: bool, dry_run: bool) {
    {
        let mut running = app.backfills.0.lock().await;
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

    let app = Arc::clone(app);
    tokio::spawn(async move {
        let outcome = run_new(&app, mine_only, dry_run, status.as_ref()).await;
        if let Some(status) = &status {
            let _ = status.edit(outcome.as_str()).await;
        }
        LogLine::new(Tone::Info, "backfill", "new").body(&outcome).print();
        app.backfills.0.lock().await.remove(&NEW_SCAN);
    });
}

/// The body of a `new` scan. Returns the line to leave in the status message.
pub(super) async fn run_new(
    app: &Arc<App>,
    mine_only: bool,
    dry_run: bool,
    status: Option<&Message>,
) -> String {
    let scan = match list_dialogs(&app.tg).await {
        Ok(scan) => scan,
        Err(e) => return format!("backfill new: cannot read the dialog list — {e}"),
    };
    let logged = match app.db.logged_chat_ids().await {
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
            !logged.contains(&d.chat_id) && !app.is_log_ignored(d.chat_id)
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
    LogLine::new(Tone::Info, "backfill", "new").body(&census).print();

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
            LogLine::new(Tone::Info, "backfill", dialog.chat_id)
                .body(&format!("would walk {}", dialog.title))
                .print();
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
        if !app.backfills.0.lock().await.insert(dialog.chat_id) {
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
        let outcome = walk_chat(
            app,
            dialog.peer,
            dialog.chat_id,
            mine_only,
            dialog.bot,
            // A dialog `new` picked has no row in the log to carry on from.
            Start::Newest,
            status,
        )
        .await;
        LogLine::new(Tone::Info, "backfill", dialog.chat_id).body(&outcome.to_string()).print();
        written += outcome.written;
        refused += usize::from(outcome.refused);
        done += 1;
        app.backfills.0.lock().await.remove(&dialog.chat_id);
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

/// Every chat in the dialog list: the main list and the archive, which is
/// most of what the official client's export finds and a scan of one folder
/// does not.
pub(super) async fn list_dialogs(client: &Client) -> Result<Scan, Box<dyn std::error::Error>> {
    let mut scan = Scan::default();
    let mut seen: HashSet<i64> = HashSet::new();
    for folder_id in [MAIN_FOLDER, ARCHIVE_FOLDER] {
        let mut pages = Pages::new(client, folder_id);
        while let Some(page) = pages.next().await? {
            for dialog in &page.dialogs {
                let Some((peer, _)) = dialog_offset(dialog) else {
                    scan.peerless += 1;
                    continue;
                };
                let Some(dialog) = describe(&peer, &page.chats, &page.users) else {
                    scan.unreadable += 1;
                    continue;
                };
                if !seen.insert(dialog.chat_id) {
                    continue;
                }
                scan.dialogs.push(dialog);
            }
        }
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
