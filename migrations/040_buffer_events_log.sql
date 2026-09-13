-- A Buffer in front of `events_log`, so a message is no longer a part.
--
-- WHAT WAS WRONG
--
-- Every event is one INSERT, and in ClickHouse one INSERT is one part. The bot
-- logs ~30,000 rows a day -- a send, an edit, a reaction, a view count -- and
-- writes each one on its own, so the table takes ~30,000 parts a day and the
-- background merges spend the day collapsing them back down to the eight the
-- data actually occupies. Nothing is broken by this. It is simply the same work
-- done over and over for rows that could have arrived together.
--
-- The bot used to solve this itself, with a write-behind buffer in process
-- memory flushed on a minute tick. That is this table's job, not the bot's, and
-- doing it in the bot meant a minute of the log living somewhere a crash drops
-- it, plus the bookkeeping to keep a lookup landing mid-flush from writing the
-- same row twice.
--
-- WHAT THIS DOES
--
-- `events_log_buffer` is a Buffer table whose destination is `events_log`
-- itself. The bot inserts into the Buffer; ClickHouse holds the rows in memory
-- and writes them down as one part when 60 seconds pass or 10,000 rows pile up.
-- At this volume that is a part a minute rather than twenty.
--
-- WHY NOTHING ELSE MOVES
--
-- `events_log` is not renamed, not rebuilt, not touched. That matters because
-- four materialized views hang off it -- mv_events_chat_stat, mv_events_daily_stat,
-- mv_events_edit_chain_stat, mv_events_user_stat -- and a materialized view is
-- an AFTER INSERT trigger on its source table. A Buffer flush is an ordinary
-- INSERT into the destination, so all four go on firing exactly as they do now,
-- on batches instead of single rows. Had the Buffer taken over the NAME instead
-- (`events_log` the Buffer, the MergeTree renamed beside it) the writes would
-- no longer land in the table the views are attached to, and all four would
-- have to be dropped and recreated against the renamed table -- the UUID
-- problem 039 had to work around. None of that is needed here.
--
-- Readers split cleanly:
--
--   * The bot's own read-after-write lookups -- find_message, find_target,
--     message_exists, poll_info::load, reply_preview -- query the BUFFER,
--     because a SELECT on a Buffer table reads the buffer and the destination
--     both, and those five all read back a message logged moments earlier.
--   * Everything else -- Grafana, the aggregate tables, ad-hoc queries -- goes
--     on querying `events_log` and is at most 60 seconds behind. For a
--     dashboard that is not a difference.
--
-- WHAT IT COSTS
--
-- Up to a minute of events lives in ClickHouse's memory rather than on disk, so
-- a hard crash of the SERVER can lose them; an ordinary restart, a DETACH or a
-- DROP flushes cleanly. The bot crashing loses nothing at all now, which is
-- strictly better than the in-process buffer this replaces.
--
-- Two Buffer limitations, and where each lands:
--
--   * FINAL is not applied to buffered rows. `events_log` is a
--     ReplacingMergeTree, but nothing queries it with FINAL -- the only FINAL
--     reads in the bot are against peer_cache, peer_names and the session
--     tables, none of which is buffered. Deduplication still happens on merge,
--     as it always did.
--   * A Buffer has no index, so its rows are scanned in full on every read.
--     That is a minute of events -- a few dozen rows -- on the five lookups
--     above, which is cheaper than the query they replace.
--
-- Buffer(database, table, num_layers, min_time, max_time, min_rows, max_rows, min_bytes, max_bytes)
--
-- One layer: layers exist to spread lock contention across parallel inserters,
-- and this table has exactly one writer. Flush when 60 seconds pass, 10,000
-- rows arrive or 10 MB accumulate; never sooner than 10 seconds, 100 rows and
-- 64 KB together. `raw` makes these rows fat, which is why the byte ceiling is
-- the one most likely to fire during a busy chat -- as intended.
CREATE TABLE IF NOT EXISTS events_log_buffer AS events_log
ENGINE = Buffer(
    currentDatabase(),
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);
