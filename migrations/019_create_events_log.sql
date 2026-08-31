-- One table for every message event the account sees.
--
-- Until now the same message was spread over four tables: `chats_log` (received),
-- `telegram_messages_new` (sent by this account), `edited_log`, `deleted_log`, plus
-- `media_log` for whatever was archived out of it. A message's life had to be
-- reassembled by joining all five.
--
-- `events_log` is append-only, one row per event, `event` naming which: 'send',
-- 'edit' or 'delete'. Media is not an event of its own — a photo is an ordinary
-- message that carries a file instead of text, so it is the send row that carries
-- `media_type` / `file_name` / `mime_type` / `size`, and `raw` keeps the message
-- object behind the text representation in `message`.
--
-- The archiver runs after the message is already logged, so it writes the same row
-- again with `sha256` / `s3_*` filled and a newer `version`; ReplacingMergeTree(version)
-- collapses the two onto one row per message. That is also why `date_time` is part
-- of the sort key: a send row keeps the message's own date and is therefore stable
-- under rewrites, while each edit and each delete has a time of its own and stays a
-- separate row.
--
-- The migration creates the table and the aggregates over it. The old tables keep
-- their names and their history and are not touched, not renamed and not
-- backfilled; they simply stop being written, and their materialized views are
-- left attached to them, so the old aggregates keep what they hold and stop
-- advancing. Nothing here is backfilled either: `events_log` and its aggregates
-- start together, at the moment this runs. Moving the Grafana boards over is a
-- step of its own.

SET allow_suspicious_low_cardinality_types = 1;

CREATE TABLE IF NOT EXISTS events_log
(
    date_time        DateTime,
    event            LowCardinality(String),
    chat_id          Int64,
    chat_title       LowCardinality(String),
    message_id       Int64,
    -- The text representation of whatever the message is: its text, a description
    -- of its media, or the service action it announces. For a delete, the text the
    -- message had while it existed.
    message          String,
    user_id          UInt64,
    username         Array(String),
    first_name       String,
    second_name      String,
    community_tag    LowCardinality(String),
    chat_usernames   Array(LowCardinality(String)),
    -- The message this one replies to, and who sent that message.
    reply_to         UInt64,
    reply_to_user_id UInt64,
    -- The forum topic it was posted in: 0 and empty outside a forum. A delete
    -- copies both off the send row it refers to.
    topic_id         Int32,
    topic_name       LowCardinality(String),
    -- This account is the sender.
    out              Bool,
    -- The message object as Telegram sent it.
    raw              String,
    -- Edits only: the unified diff against the text this edit replaced. That text
    -- is the `message` of the send — or of the previous edit — of the same message,
    -- so it is not stored a second time.
    diff             String,
    -- What the message carries besides text, and where the file was archived.
    media_type       LowCardinality(String),
    file_name        String,
    mime_type        LowCardinality(String),
    size             UInt64,
    sha256           String,
    s3_bucket        LowCardinality(String),
    s3_key           String,
    -- Ingest time: the newest write of a key wins.
    version          DateTime
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(date_time)
ORDER BY (chat_id, message_id, event, date_time);


-- ---------------------------------------------------------------------------
-- The aggregates.
--
-- The old set (`mv_chat_stat`, `mv_user_stat`, `mv_message_stats`,
-- `mv_edited_chain_stats`) stays attached to the old tables: it keeps the history
-- it already holds and stops advancing once the bot writes only here. Nothing
-- below touches it.
--
-- What follows is a fresh set over `events_log`, and it can say more than the old
-- one could: sends, edits and deletes are rows of one table now, so a chat's
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
-- ---------------------------------------------------------------------------

-- ---------------------------------------------------------------------------
-- Per chat.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_chat_stat
(
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
ORDER BY chat_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_chat_stat TO events_chat_stat AS
SELECT
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
GROUP BY chat_id;

CREATE VIEW IF NOT EXISTS v_chat_stat AS
SELECT
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
GROUP BY chat_id;


-- ---------------------------------------------------------------------------
-- Per user. Deletions are left out: Telegram does not say who deleted a message,
-- so their `user_id` is 0 and they would all pile up under one phantom user.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_user_stat
(
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
ORDER BY user_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_user_stat TO events_user_stat AS
SELECT
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
GROUP BY user_id;

CREATE VIEW IF NOT EXISTS v_user_stat AS
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
FROM events_user_stat
GROUP BY user_id;


-- ---------------------------------------------------------------------------
-- Per day. One row per client / day / chat / topic / event: the shape the boards
-- plot, and the one the old tables could not produce without a union of three of
-- them. `topic_id` is 0 outside a forum, so a chat that has no topics is one row
-- per day per event exactly as before.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events_daily_stat
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
PARTITION BY toYYYYMM(day)
ORDER BY (day, chat_id, topic_id, event);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_events_daily_stat TO events_daily_stat AS
SELECT
    toDate(date_time) AS day,
    chat_id,
    topic_id,
    event,
    countState() AS events,
    groupUniqArrayState(user_id) AS senders,
    sumState(size) AS media_bytes
FROM events_log
WHERE s3_key = ''
GROUP BY day, chat_id, topic_id, event;

CREATE VIEW IF NOT EXISTS v_daily_stat AS
SELECT
    day,
    chat_id,
    topic_id,
    event,
    countMerge(events) AS events,
    length(groupUniqArrayMerge(senders)) AS senders,
    sumMerge(media_bytes) AS media_bytes
FROM events_daily_stat
GROUP BY day, chat_id, topic_id, event;


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
