//! `backfill_state`: the id range each walk has covered.

use super::*;

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
pub(super) async fn mark_walked(chat_id: i64, mine_only: bool, finished: bool) -> String {
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
pub(super) struct Covered {
    pub(super) min_id: i64,
    pub(super) max_id: i64,
    /// Whether that walk reached the start of the history. When it did there is
    /// nothing under `min_id` to go back for.
    pub(super) complete: bool,
}

/// What a chat's earlier walks covered, for the messages this one is after.
///
/// A `mine` walk reads the `all` row as well: everything an `all` walk stored
/// covers this account's messages inside the same range too. An `all` walk reads
/// only its own — a `mine` row says nothing about everyone else's messages.
pub(super) async fn covered(chat_id: i64, mine_only: bool) -> Option<Covered> {
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
pub(super) struct CoveredRow {
    pub(super) chat_id: i64,
    pub(super) mine_only: bool,
    pub(super) min_id: i64,
    pub(super) max_id: i64,
    pub(super) complete: bool,
    pub(super) messages: u64,
    pub(super) walked_at: u32,
}

/// Write down what is covered now, so the next walk can jump over it.
pub(super) async fn record(chat_id: i64, mine_only: bool, covered: Covered, messages: u64) {
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
