-- What else can happen to a message after it was sent.
--
-- Until now the log knew four things that happen to a message -- it is sent,
-- edited, deleted, reacted to -- plus the service message a group posts when
-- one is pinned. Telegram reports more than that out of band, and all of it was
-- being dropped on the floor:
--
--   * `pin` / `unpin`. A pin in a group arrives as a service message too, and
--     that is what `event = 'service'` has been recording. Nothing announces an
--     *un*pin, and neither a channel nor a private chat announces a pin at all,
--     so for those the update is the only evidence there is.
--
--   * `poll`. A poll's results change as people vote, and Telegram sends the
--     whole result set rather than the vote -- the same shape as a reaction
--     update. The send row keeps the question and the options as the poll was
--     created; these rows keep what the counts became.
--
--   * `views`. A channel post's view and forward counters. Telegram reports
--     each on its own and reports a total rather than a delta, so a row is the
--     count as it stands and the counter that update did not carry stays 0.
--
-- None of these is a message, and none of them is counted as one: the
-- aggregates count by event name, and the names are new.
--
-- The new columns are nullable in spirit, not in type -- an event that does not
-- use one leaves it at its zero value, as every other event already does.

-- ---------------------------------------------------------------------------
-- 1. The columns the new events fill.
--
--    `poll_id` is filled on the send row as well: a results update names the
--    message only when Telegram feels like it, and when it does not, the poll
--    id is the only way back to the message the poll is on.
--
--    Codecs follow 034: an id that recurs gets ZSTD(9), a small counter is left
--    on LZ4, where a codec would cost more in metadata than it saves.
-- ---------------------------------------------------------------------------

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS poll_id           Int64 CODEC(ZSTD(9)),
    ADD COLUMN IF NOT EXISTS poll_results      Map(String, UInt32),
    ADD COLUMN IF NOT EXISTS poll_total_voters UInt32,
    ADD COLUMN IF NOT EXISTS views             UInt32,
    ADD COLUMN IF NOT EXISTS forwards          UInt32;

-- ---------------------------------------------------------------------------
-- 2. Keep the edit chain to the message's own life.
--
--    `events_edit_chain_stat` is one row per message, counting the versions it
--    went through. A pin or a view counter is not a version, and a row keyed by
--    a message that only ever appeared in a `views` update is not an edit chain
--    at all -- it would be a row of zeroes whose `first_seen` is the day someone
--    scrolled past the post. So the view now names the three events a message's
--    life is made of instead of excluding `file_uploaded` by name.
--
--    Only the view is replaced; the table keeps the rows it has. The excluded
--    events never contributed to `versions`, `edits`, `last_edit` or `deleted`,
--    so the only trace they left is a `first_seen` that may be earlier than the
--    message's own -- which was already true of the rows written before this.
-- ---------------------------------------------------------------------------

DROP VIEW IF EXISTS telegram_user_bot.mv_events_edit_chain_stat;

CREATE MATERIALIZED VIEW telegram_user_bot.mv_events_edit_chain_stat
TO telegram_user_bot.events_edit_chain_stat AS
SELECT
    chat_id,
    message_id,
    countIfState(event IN ('send', 'edit')) AS versions,
    countIfState(event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxIfState(date_time, event = 'edit') AS last_edit,
    maxIfState(date_time, event = 'delete') AS deleted
FROM telegram_user_bot.events_log
WHERE NOT ephemeral AND (event IN ('send', 'edit', 'delete'))
GROUP BY chat_id, message_id;
