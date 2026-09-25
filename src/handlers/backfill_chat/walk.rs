//! Walking one chat's history and writing what the log is missing.

use super::*;

/// What one segment of a walk read.
#[derive(Default)]
pub(super) struct Read {
    pub(super) seen: usize,
    pub(super) pinged: usize,
    /// The ids at either end of what this segment actually read, 0 while it has
    /// read nothing.
    pub(super) lowest: i64,
    pub(super) highest: i64,
    /// Telegram's refusal, if the segment ended on one.
    pub(super) error: Option<String>,
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
pub(super) async fn next_id_below(client: &Client, peer: PeerRef, anchor: i64) -> Option<i64> {
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
pub(super) async fn walk(
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
        if read.seen % PROGRESS_EVERY == 0 {
            if let Some(status) = status {
                let _ = status
                    .edit(format!(
                        "backfill {chat_id}: {}/{total} read, {written} written…",
                        read.seen
                    ))
                    .await;
            }
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
pub(super) async fn run(
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

/// Walk one chat's history, writing every message the log is missing, and skip
/// whatever an earlier walk already covered. Returns the line to show for it.
///
/// Two stretches at most: from the newest message down to the top of the covered
/// range, then — only if that earlier walk never reached the start of the history
/// — from its bottom downwards. `full` ignores the covered range and reads
/// everything, which is the way back if a recorded range is ever wrong.
#[allow(clippy::too_many_arguments)]
pub(super) async fn walk_chat(
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
pub(super) fn is_health_check(text: &str) -> bool {
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
pub(super) async fn is_bot_chat(peer: PeerRef) -> bool {
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
pub(super) struct Outcome {
    pub(super) written: usize,
    /// Whether the walk ended on Telegram saying no — a left chat it will not
    /// hand the history of, most of the time.
    pub(super) refused: bool,
    pub(super) line: String,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

/// Turn the messages the log does not have yet into rows.
pub(super) async fn convert(
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
pub(super) async fn collect(building: &mut JoinSet<Event>, batch: &mut Vec<Event>) {
    match building.join_next().await {
        Some(Ok(event)) => batch.push(event),
        Some(Err(e)) => warn!("backfill: building a row: {e}"),
        None => {}
    }
}

/// Which of these message ids the log already holds, asked a batch at a time.
/// Read through the Buffer, so a message logged a moment ago counts.
pub(super) async fn known_ids(chat_id: i64, ids: &[i64]) -> HashSet<i64> {
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

pub(super) async fn flush(batch: &mut Vec<Event>, written: &mut usize) {
    if batch.is_empty() {
        return;
    }
    match crate::db::insert_rows("events_log", batch).await {
        Ok(()) => *written += batch.len(),
        Err(e) => warn!("backfill: insert: {e}"),
    }
    batch.clear();
}
