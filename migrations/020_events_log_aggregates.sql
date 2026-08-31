-- The aggregates, rebuilt on `events_log`.
--
-- The old set (`mv_chat_stat`, `mv_user_stat`, `mv_message_stats`,
-- `mv_edited_chain_stats`) reads the old tables and stays attached to them: it
-- keeps the history it already holds and stops advancing once the bot writes only
-- into `events_log`. Nothing here touches it.
--
-- What is below is a fresh set over the new table, and it can say more than the old
-- one could: sends, edits and deletes are now rows of one table, so a chat's
-- counters no longer come from three places that could not be joined, and a daily
-- rollup — the thing the boards actually plot — becomes a single view.
--
-- Two rules run through all of them:
--
--   * `WHERE s3_key = ''` — the archiver writes the send row a second time once the
--     file is in S3, with the same key and a newer version. ReplacingMergeTree
--     collapses that at merge time, but a materialized view fires per insert and
--     would count the message twice, so the enrichment write is skipped. It carries
--     nothing the counters need: `media_type` and `size` are already on the first
--     write, only `sha256` / `s3_bucket` / `s3_key` arrive with the second.
--
--   * every counter is an `-If` state over one scan of the row rather than one view
--     per event, so a chat's sends, edits and deletes stay in one row.
--
-- Nothing is backfilled: they start at the moment `events_log` does.

SET allow_suspicious_low_cardinality_types = 1;


-- ---------------------------------------------------------------------------
-- Per chat.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_chat_stat
(
    client_id       UInt64,
    chat_id         Int64,
    last_title      AggregateFunction(anyLastIf, String, UInt8),
    messages        AggregateFunction(countIf, UInt8),
    outgoing        AggregateFunction(countIf, UInt8),
    replies         AggregateFunction(countIf, UInt8),
    media_messages  AggregateFunction(countIf, UInt8),
    media_bytes     AggregateFunction(sumIf, UInt64, UInt8),
    edits           AggregateFunction(countIf, UInt8),
    deletes         AggregateFunction(countIf, UInt8),
    participants    AggregateFunction(groupUniqArrayIf, UInt64, UInt8),
    last_message_id AggregateFunction(maxIf, Int64, UInt8),
    first_seen      AggregateFunction(min, DateTime),
    last_seen       AggregateFunction(max, DateTime)
)
ENGINE = AggregatingMergeTree
ORDER BY (client_id, chat_id);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_chat_stat TO events_chat_stat AS
SELECT
    client_id,
    chat_id,
    -- Only a send knows the chat's title; an edit or a delete leaves it empty and
    -- must not be allowed to overwrite it.
    anyLastIfState(toString(chat_title), chat_title != '') AS last_title,
    countIfState(event = 'send') AS messages,
    countIfState(toUInt8((event = 'send') AND out)) AS outgoing,
    countIfState((event = 'send') AND (reply_to != 0)) AS replies,
    countIfState((event = 'send') AND (media_type != '')) AS media_messages,
    sumIfState(size, event = 'send') AS media_bytes,
    countIfState(event = 'edit') AS edits,
    countIfState(event = 'delete') AS deletes,
    groupUniqArrayIfState(user_id, (event = 'send') AND (user_id != 0)) AS participants,
    maxIfState(message_id, event = 'send') AS last_message_id,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM events_log
WHERE s3_key = ''
GROUP BY client_id, chat_id;

CREATE VIEW IF NOT EXISTS v_chat_stat AS
SELECT
    client_id,
    chat_id,
    anyLastIfMerge(last_title) AS chat_title,
    countIfMerge(messages) AS messages,
    countIfMerge(outgoing) AS outgoing,
    countIfMerge(replies) AS replies,
    countIfMerge(media_messages) AS media_messages,
    sumIfMerge(media_bytes) AS media_bytes,
    countIfMerge(edits) AS edits,
    countIfMerge(deletes) AS deletes,
    length(groupUniqArrayIfMerge(participants)) AS participants,
    maxIfMerge(last_message_id) AS last_message_id,
    minMerge(first_seen) AS first_seen,
    maxMerge(last_seen) AS last_seen
FROM events_chat_stat
GROUP BY client_id, chat_id;


-- ---------------------------------------------------------------------------
-- Per user. Deletions are left out: Telegram does not say who deleted a message,
-- so their `user_id` is 0 and they would all pile up under one phantom user.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_user_stat
(
    client_id    UInt64,
    user_id      UInt64,
    username     AggregateFunction(anyLastIf, Array(String), UInt8),
    first_name   AggregateFunction(anyLastIf, String, UInt8),
    second_name  AggregateFunction(anyLastIf, String, UInt8),
    chats        AggregateFunction(groupUniqArray, Int64),
    messages     AggregateFunction(countIf, UInt8),
    replies      AggregateFunction(countIf, UInt8),
    media_messages AggregateFunction(countIf, UInt8),
    edits        AggregateFunction(countIf, UInt8),
    first_seen   AggregateFunction(min, DateTime),
    last_seen    AggregateFunction(max, DateTime)
)
ENGINE = AggregatingMergeTree
ORDER BY (client_id, user_id);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_user_stat TO events_user_stat AS
SELECT
    client_id,
    user_id,
    -- Same reason as the chat title: an edit row carries no identity, so it must
    -- not blank out what the sends know.
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
FROM events_log
WHERE (s3_key = '') AND (event != 'delete') AND (user_id != 0)
GROUP BY client_id, user_id;

CREATE VIEW IF NOT EXISTS v_user_stat AS
SELECT
    client_id,
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
FROM events_user_stat
GROUP BY client_id, user_id;


-- ---------------------------------------------------------------------------
-- Per day. One row per client / day / chat / topic / event: the shape the boards
-- plot, and the one the old tables could not produce without a union of three of
-- them. `topic_id` is 0 outside a forum, so a chat that has no topics is one row
-- per day per event exactly as before.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_daily_stat
(
    client_id   UInt64,
    day         Date,
    chat_id     Int64,
    topic_id    Int32,
    event       LowCardinality(String),
    events      AggregateFunction(count),
    senders     AggregateFunction(groupUniqArray, UInt64),
    media_bytes AggregateFunction(sum, UInt64)
)
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(day)
ORDER BY (client_id, day, chat_id, topic_id, event);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_daily_stat TO events_daily_stat AS
SELECT
    client_id,
    toDate(date_time) AS day,
    chat_id,
    topic_id,
    event,
    countState() AS events,
    groupUniqArrayState(user_id) AS senders,
    sumState(size) AS media_bytes
FROM events_log
WHERE s3_key = ''
GROUP BY client_id, day, chat_id, topic_id, event;

CREATE VIEW IF NOT EXISTS v_daily_stat AS
SELECT
    client_id,
    day,
    chat_id,
    topic_id,
    event,
    countMerge(events) AS events,
    length(groupUniqArrayMerge(senders)) AS senders,
    sumMerge(media_bytes) AS media_bytes
FROM events_daily_stat
GROUP BY client_id, day, chat_id, topic_id, event;


-- ---------------------------------------------------------------------------
-- Edit chains. The old `edited_chain_stats` counted edit rows only, so a message
-- edited once showed a chain of one and the send it started from had to be
-- subtracted by hand in the reading view. Here the send row is counted with them,
-- so `versions` is the number of texts the message actually had.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_edit_chain_stat
(
    chat_id     Int64,
    message_id  Int64,
    versions    AggregateFunction(countIf, UInt8),
    edits       AggregateFunction(countIf, UInt8),
    first_seen  AggregateFunction(min, DateTime),
    last_edit   AggregateFunction(maxIf, DateTime, UInt8),
    deleted     AggregateFunction(maxIf, DateTime, UInt8)
)
ENGINE = AggregatingMergeTree
ORDER BY (chat_id, message_id);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_edit_chain_stat TO events_edit_chain_stat AS
SELECT
    chat_id,
    message_id,
    countIfState(event != 'delete') AS versions,
    countIfState(event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxIfState(date_time, event = 'edit') AS last_edit,
    maxIfState(date_time, event = 'delete') AS deleted
FROM events_log
WHERE s3_key = ''
GROUP BY chat_id, message_id;

CREATE VIEW IF NOT EXISTS v_edit_chain_stat AS
SELECT
    chat_id,
    message_id,
    countIfMerge(versions) AS versions,
    countIfMerge(edits) AS edits,
    minMerge(first_seen) AS first_seen,
    maxIfMerge(last_edit) AS last_edit,
    maxIfMerge(deleted) AS deleted
FROM events_edit_chain_stat
GROUP BY chat_id, message_id;
