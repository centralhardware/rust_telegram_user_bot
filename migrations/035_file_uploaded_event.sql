-- The archiver stops rewriting the send row, and the counters stop remembering
-- every message they ever counted.
--
-- Until now a file archived to S3 was logged as the message's own send row
-- written a second time, with `sha256` / `s3_*` filled and a newer `version` for
-- the ReplacingMergeTree to collapse onto the original. It worked, but it meant
-- one message could be two rows in flight, and a materialized view -- which
-- fires per INSERT, long before any merge -- had no way to tell that second row
-- from a real message. Hence `WHERE s3_key = ''` in every aggregate, and hence
-- every counter being `uniqExact` over `(message_id, event, date_time)` rather
-- than a count: the tuple was there so that a row arriving twice could be
-- recognised as the same row.
--
-- The price was that the aggregate states had to keep the identity of every
-- message ever logged, for ever. `events_daily_stat` can afford it -- a day's
-- set is bounded -- but `events_chat_stat` and `events_user_stat` are lifetime
-- rows, so their states grew with the archive and would never stop.
--
-- So the archiver now writes its own event, `file_uploaded`: a row naming the
-- message the file belongs to, carrying the file and nothing else. No row of
-- `events_log` is written twice any more, the aggregates exclude that event by
-- name instead of by `s3_key`, and every counter goes back to being a count.
-- The states become a fixed handful of bytes per row and stay that size.
--
-- What is still deduplicated, and by what:
--
--   * A redelivery after a reconnect -- Telegram replaying its backlog -- is
--     byte-for-byte the same row at the same `version`, so `ReplacingMergeTree`
--     still collapses it in the table. The counters no longer see through it:
--     the MV counts the redelivered copy again, and a chat's lifetime totals
--     can drift upward by however many rows a reconnect replayed. That is the
--     trade, made deliberately: an unbounded state for a bounded error.
--
--   * `senders` / `participants` / `chats` stay `groupUniqArray`. They are sets
--     of user and chat ids, bounded by how many people the account talks to,
--     and a duplicate row cannot inflate them.
--
--   * Media bytes stop being the set of `(message_id, size)` pairs and become a
--     plain sum. A day's archived bytes now come from the `file_uploaded` rows
--     of that day -- what was actually stored -- while the `send` rows keep
--     summing what Telegram said the file weighed.
--
-- `version` on `events_log` is left in place. Nothing raises it any more, but it
-- is what makes a redelivery a no-op rather than a second row, and dropping it
-- would give the table back the duplicates this migration is removing.
--
-- Run it with the bot stopped. Steps 2 and 6 read the table as it stands, and a
-- row inserted between them is a row the aggregates never see.

-- ---------------------------------------------------------------------------
-- 1. Unhook the aggregates, so the backfill below does not run through them.
-- ---------------------------------------------------------------------------

DROP VIEW IF EXISTS telegram_user_bot.mv_events_chat_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_user_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_daily_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_edit_chain_stat;

-- ---------------------------------------------------------------------------
-- 2. The history, converted: every send row the archiver enriched becomes the
--    `file_uploaded` row it would be written as today. `FINAL` because the
--    enriched row and the original may not have merged yet, and only the
--    enriched one carries a key.
--
--    `date_time` is the send's, not the upload's: the upload time was never
--    stored -- `version` holds it for the rows written since 019, but it is the
--    archiver's ingest second and 0 for anything older -- and the send is the
--    honest answer to "when", off by the seconds the download took.
-- ---------------------------------------------------------------------------

INSERT INTO telegram_user_bot.events_log
    (date_time, event, chat_id, chat_title, message_id, topic_id, topic_name,
     ephemeral, receiver_id, media_type, file_name, mime_type, size,
     sha256, s3_bucket, s3_key, version)
SELECT
    date_time,
    'file_uploaded' AS event,
    chat_id,
    chat_title,
    message_id,
    topic_id,
    topic_name,
    ephemeral,
    receiver_id,
    media_type,
    file_name,
    mime_type,
    size,
    sha256,
    s3_bucket,
    s3_key,
    0 AS version
FROM telegram_user_bot.events_log FINAL
WHERE (event = 'send') AND (s3_key != '');

-- ---------------------------------------------------------------------------
-- 3. And cleared off the send rows, which are messages again and nothing else.
--    The file is not lost: it is on the `file_uploaded` row inserted above.
-- ---------------------------------------------------------------------------

ALTER TABLE telegram_user_bot.events_log
    UPDATE sha256 = '', s3_bucket = '', s3_key = ''
    WHERE (event = 'send') AND (s3_key != '');

-- ---------------------------------------------------------------------------
-- 4. The state tables, rebuilt for counts.
--
--    An `AggregateFunction` column cannot be altered into a different one, and
--    every row of these tables is derivable from `events_log`, so they are
--    dropped and refilled in step 6 rather than migrated.
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS telegram_user_bot.events_chat_stat;
DROP TABLE IF EXISTS telegram_user_bot.events_user_stat;
DROP TABLE IF EXISTS telegram_user_bot.events_daily_stat;
DROP TABLE IF EXISTS telegram_user_bot.events_edit_chain_stat;

CREATE TABLE telegram_user_bot.events_chat_stat
(
    chat_id         Int64,
    last_title      AggregateFunction(anyLastIf, String, UInt8),
    messages        AggregateFunction(countIf, UInt8),
    outgoing        AggregateFunction(countIf, UInt8),
    replies         AggregateFunction(countIf, UInt8),
    media_messages  AggregateFunction(countIf, UInt8),
    edits           AggregateFunction(countIf, UInt8),
    deletes         AggregateFunction(countIf, UInt8),
    files           AggregateFunction(countIf, UInt8),
    participants    AggregateFunction(groupUniqArrayIf, UInt64, UInt8),
    last_message_id AggregateFunction(maxIf, Int64, UInt8),
    first_seen      AggregateFunction(min, DateTime),
    last_seen       AggregateFunction(max, DateTime)
)
ENGINE = AggregatingMergeTree
ORDER BY chat_id;

CREATE TABLE telegram_user_bot.events_user_stat
(
    user_id        UInt64,
    username       AggregateFunction(anyLastIf, Array(String), UInt8),
    first_name     AggregateFunction(anyLastIf, String, UInt8),
    second_name    AggregateFunction(anyLastIf, String, UInt8),
    chats          AggregateFunction(groupUniqArray, Int64),
    messages       AggregateFunction(countIf, UInt8),
    replies        AggregateFunction(countIf, UInt8),
    media_messages AggregateFunction(countIf, UInt8),
    edits          AggregateFunction(countIf, UInt8),
    first_seen     AggregateFunction(min, DateTime),
    last_seen      AggregateFunction(max, DateTime)
)
ENGINE = AggregatingMergeTree
ORDER BY user_id;

CREATE TABLE telegram_user_bot.events_daily_stat
(
    day         Date,
    chat_id     Int64,
    topic_id    Int32,
    event       LowCardinality(String),
    events      AggregateFunction(count),
    senders     AggregateFunction(groupUniqArray, UInt64),
    media_bytes AggregateFunction(sum, UInt64)
)
ENGINE = AggregatingMergeTree
ORDER BY (day, chat_id, topic_id, event);

CREATE TABLE telegram_user_bot.events_edit_chain_stat
(
    chat_id    Int64,
    message_id Int64,
    versions   AggregateFunction(countIf, UInt8),
    edits      AggregateFunction(countIf, UInt8),
    first_seen AggregateFunction(min, DateTime),
    last_edit  AggregateFunction(maxIf, DateTime, UInt8),
    deleted    AggregateFunction(maxIf, DateTime, UInt8)
)
ENGINE = AggregatingMergeTree
ORDER BY (chat_id, message_id);

-- ---------------------------------------------------------------------------
-- 5. The aggregates themselves.
--
--    `WHERE event != 'file_uploaded'` replaces `WHERE s3_key = ''`: it says the
--    same thing -- do not count the archiver's row as traffic -- but by naming
--    the event rather than by noticing that a column happens to be filled.
--    The chat aggregate keeps that row as `files`, which is the one counter it
--    can now have and could not before.
-- ---------------------------------------------------------------------------

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_chat_stat
TO telegram_user_bot.events_chat_stat AS
SELECT
    chat_id,
    -- Only a send knows the chat's title; an edit or a delete leaves it empty
    -- and must not be allowed to overwrite it.
    anyLastIfState(toString(chat_title), chat_title != '') AS last_title,
    countIfState(event = 'send') AS messages,
    countIfState(toUInt8((event = 'send') AND out)) AS outgoing,
    countIfState((event = 'send') AND (reply_to != 0)) AS replies,
    countIfState((event = 'send') AND (media_type != '')) AS media_messages,
    countIfState(event = 'edit') AS edits,
    countIfState(event = 'delete') AS deletes,
    countIfState(event = 'file_uploaded') AS files,
    groupUniqArrayIfState(user_id, (event = 'send') AND (user_id != 0)) AS participants,
    maxIfState(message_id, event = 'send') AS last_message_id,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM telegram_user_bot.events_log
WHERE NOT ephemeral
GROUP BY chat_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_user_stat
TO telegram_user_bot.events_user_stat AS
SELECT
    user_id,
    anyLastIfState(username, notEmpty(username)) AS username,
    anyLastIfState(first_name, first_name != '') AS first_name,
    anyLastIfState(second_name, second_name != '') AS second_name,
    groupUniqArrayState(chat_id) AS chats,
    countIfState(event = 'send') AS messages,
    countIfState((event = 'send') AND (reply_to != 0)) AS replies,
    countIfState((event = 'send') AND (media_type != '')) AS media_messages,
    countIfState(event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM telegram_user_bot.events_log
WHERE NOT ephemeral AND (event NOT IN ('delete', 'file_uploaded')) AND (user_id != 0)
GROUP BY user_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_daily_stat
TO telegram_user_bot.events_daily_stat AS
SELECT
    toDate(date_time) AS day,
    chat_id,
    topic_id,
    event,
    countState() AS events,
    groupUniqArrayState(user_id) AS senders,
    sumState(size) AS media_bytes
FROM telegram_user_bot.events_log
WHERE NOT ephemeral
GROUP BY day, chat_id, topic_id, event;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_edit_chain_stat
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
WHERE NOT ephemeral AND (event != 'file_uploaded')
GROUP BY chat_id, message_id;

-- ---------------------------------------------------------------------------
-- 6. The history, counted once.
--
--    `FINAL` is what makes this safe to run on a table whose duplicates have
--    not all been merged away yet: a redelivered row that is still its own part
--    would otherwise be counted twice into states that can no longer tell.
-- ---------------------------------------------------------------------------

INSERT INTO telegram_user_bot.events_chat_stat
SELECT
    chat_id,
    anyLastIfState(toString(chat_title), chat_title != '') AS last_title,
    countIfState(event = 'send') AS messages,
    countIfState(toUInt8((event = 'send') AND out)) AS outgoing,
    countIfState((event = 'send') AND (reply_to != 0)) AS replies,
    countIfState((event = 'send') AND (media_type != '')) AS media_messages,
    countIfState(event = 'edit') AS edits,
    countIfState(event = 'delete') AS deletes,
    countIfState(event = 'file_uploaded') AS files,
    groupUniqArrayIfState(user_id, (event = 'send') AND (user_id != 0)) AS participants,
    maxIfState(message_id, event = 'send') AS last_message_id,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM telegram_user_bot.events_log FINAL
WHERE NOT ephemeral
GROUP BY chat_id;

INSERT INTO telegram_user_bot.events_user_stat
SELECT
    user_id,
    anyLastIfState(username, notEmpty(username)) AS username,
    anyLastIfState(first_name, first_name != '') AS first_name,
    anyLastIfState(second_name, second_name != '') AS second_name,
    groupUniqArrayState(chat_id) AS chats,
    countIfState(event = 'send') AS messages,
    countIfState((event = 'send') AND (reply_to != 0)) AS replies,
    countIfState((event = 'send') AND (media_type != '')) AS media_messages,
    countIfState(event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM telegram_user_bot.events_log FINAL
WHERE NOT ephemeral AND (event NOT IN ('delete', 'file_uploaded')) AND (user_id != 0)
GROUP BY user_id;

INSERT INTO telegram_user_bot.events_daily_stat
SELECT
    toDate(date_time) AS day,
    chat_id,
    topic_id,
    event,
    countState() AS events,
    groupUniqArrayState(user_id) AS senders,
    sumState(size) AS media_bytes
FROM telegram_user_bot.events_log FINAL
WHERE NOT ephemeral
GROUP BY day, chat_id, topic_id, event;

INSERT INTO telegram_user_bot.events_edit_chain_stat
SELECT
    chat_id,
    message_id,
    countIfState(event IN ('send', 'edit')) AS versions,
    countIfState(event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxIfState(date_time, event = 'edit') AS last_edit,
    maxIfState(date_time, event = 'delete') AS deleted
FROM telegram_user_bot.events_log FINAL
WHERE NOT ephemeral AND (event != 'file_uploaded')
GROUP BY chat_id, message_id;

-- ---------------------------------------------------------------------------
-- 7. The reading views, following their states.
-- ---------------------------------------------------------------------------

CREATE OR REPLACE VIEW telegram_user_bot.v_chat_stat AS
SELECT
    chat_id,
    anyLastIfMerge(last_title) AS chat_title,
    countIfMerge(messages) AS messages,
    countIfMerge(outgoing) AS outgoing,
    countIfMerge(replies) AS replies,
    countIfMerge(media_messages) AS media_messages,
    countIfMerge(edits) AS edits,
    countIfMerge(deletes) AS deletes,
    countIfMerge(files) AS files,
    length(groupUniqArrayIfMerge(participants)) AS participants,
    maxIfMerge(last_message_id) AS last_message_id,
    minMerge(first_seen) AS first_seen,
    maxMerge(last_seen) AS last_seen
FROM telegram_user_bot.events_chat_stat
GROUP BY chat_id;

CREATE OR REPLACE VIEW telegram_user_bot.v_user_stat AS
SELECT
    user_id,
    anyLastIfMerge(username) AS username,
    anyLastIfMerge(first_name) AS first_name,
    anyLastIfMerge(second_name) AS second_name,
    length(groupUniqArrayMerge(chats)) AS chats,
    countIfMerge(messages) AS messages,
    countIfMerge(replies) AS replies,
    countIfMerge(media_messages) AS media_messages,
    countIfMerge(edits) AS edits,
    minMerge(first_seen) AS first_seen,
    maxMerge(last_seen) AS last_seen
FROM telegram_user_bot.events_user_stat
GROUP BY user_id;

CREATE OR REPLACE VIEW telegram_user_bot.v_daily_stat AS
SELECT
    day,
    chat_id,
    topic_id,
    event,
    countMerge(events) AS events,
    length(groupUniqArrayMerge(senders)) AS senders,
    sumMerge(media_bytes) AS media_bytes
FROM telegram_user_bot.events_daily_stat
GROUP BY day, chat_id, topic_id, event;

CREATE OR REPLACE VIEW telegram_user_bot.v_edit_chain_stat AS
SELECT
    chat_id,
    message_id,
    countIfMerge(versions) AS versions,
    countIfMerge(edits) AS edits,
    minMerge(first_seen) AS first_seen,
    maxIfMerge(last_edit) AS last_edit,
    maxIfMerge(deleted) AS deleted
FROM telegram_user_bot.events_edit_chain_stat
GROUP BY chat_id, message_id;
