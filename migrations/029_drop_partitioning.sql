-- `events_log` and `events_daily_stat` stop being partitioned.
--
-- Migration 019 gave both `PARTITION BY toYYYYMM(...)`, copied from the legacy
-- tables it replaced. At this volume that is cost without benefit. The whole
-- history of the account -- the five legacy tables together, everything before
-- 2026-08-31 -- is 11M rows; `events_log` itself holds 28k. A month of it is a
-- few hundred thousand rows at most, far below the size a MergeTree part wants
-- to be, so monthly partitioning only means more parts, more merges and more
-- open files.
--
-- Nor does it prune anything the sort key does not prune already: every query
-- the boards run reaches the table through `chat_id`, which leads the ORDER BY,
-- and a time filter on top of it reads a handful of granules either way. And
-- nothing here is ever dropped by month -- there is no retention on this data,
-- it is the archive.
--
-- So: no PARTITION BY at all. If a year ever has to be detached or moved to
-- cold storage, a TTL expression can do it without the table being cut into
-- pieces for the rest of its life.
--
-- `PARTITION BY` cannot be altered, so each table is rebuilt beside itself and
-- swapped in. The bot keeps writing throughout: EXCHANGE is atomic, and the
-- rows that land in the old table between the copy and the swap are re-inserted
-- from it afterwards. `ReplacingMergeTree(version)` collapses those onto the
-- copies already there -- the same property that makes a reconnect's replayed
-- backlog harmless (see 019).
--
-- The aggregates are dropped and recreated around the swap, because a
-- materialized view follows its source table by UUID and EXCHANGE swaps the
-- UUIDs: left in place they would keep reading the table that is on its way to
-- being dropped. Their storage tables are not touched and keep their history;
-- `events_daily_stat` is the exception, since it is the one being rebuilt, and
-- it is repopulated from `events_log` in full. Double counting is not a concern
-- for any of them: every counter is `uniqExact` over the row's identity, so
-- re-inserting a row that was already counted changes nothing.

-- ---------------------------------------------------------------------------
-- 1. Unhook the aggregates.
-- ---------------------------------------------------------------------------

DROP VIEW IF EXISTS telegram_user_bot.mv_events_chat_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_user_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_daily_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_edit_chain_stat;

-- ---------------------------------------------------------------------------
-- 2. `events_log`, rebuilt unpartitioned.
--
--    The column list is the table as it stands after 023, 025, 027 and 028 --
--    it is 019's, plus the reply-quote, comment and service-message columns
--    those added.
-- ---------------------------------------------------------------------------

SET allow_suspicious_low_cardinality_types = 1;

CREATE TABLE IF NOT EXISTS telegram_user_bot.events_log_unpartitioned
(
    date_time          DateTime,
    event              LowCardinality(String),
    chat_id            Int64,
    chat_title         LowCardinality(String),
    message_id         Int64,
    message            String,
    user_id            UInt64,
    username           Array(String),
    first_name         String,
    second_name        String,
    community_tag      LowCardinality(String),
    community_id       Int64,
    chat_usernames     Array(LowCardinality(String)),
    reply_to           UInt64,
    reply_to_user_id   UInt64,
    reply_to_chat_id   Int64 DEFAULT 0,
    quote_text         String DEFAULT '',
    comment_to         UInt64 DEFAULT 0,
    topic_id           Int32,
    topic_name         LowCardinality(String),
    fwd_from_user_id   UInt64,
    fwd_from_chat_id   Int64,
    fwd_from_msg_id    Int64,
    fwd_from_name      String,
    fwd_date           DateTime,
    action             LowCardinality(String),
    service_message_id Int64 DEFAULT 0,
    grouped_id         UInt64,
    reactions          Map(String, UInt32),
    ephemeral          Bool,
    receiver_id        UInt64,
    reply_to_ephemeral Bool,
    welcome            Bool,
    out                Bool,
    raw                String,
    via_bot_id         UInt64,
    post_author        String,
    guest_from_id      Int64,
    pinned             Bool,
    silent             Bool,
    noforwards         Bool,
    ttl_period         UInt32,
    diff               String COMMENT 'Edits only: a unified diff, counted in words, against the text this edit replaced. That text is not stored -- `edit_prev_text` puts it back together out of this and `message`, and `edit_diff_html` renders the marked-up version the boards print.',
    media_type         LowCardinality(String),
    file_name          String,
    mime_type          LowCardinality(String),
    size               UInt64,
    duration           UInt32,
    width              UInt32,
    height             UInt32,
    lat                Float64,
    lon                Float64,
    poll_question      String,
    poll_options       Array(String),
    sha256             String,
    s3_bucket          LowCardinality(String),
    s3_key             String,
    version            DateTime
)
ENGINE = ReplacingMergeTree(version)
ORDER BY (chat_id, ephemeral, message_id, event, date_time);

INSERT INTO telegram_user_bot.events_log_unpartitioned
SELECT * FROM telegram_user_bot.events_log;

-- The bot's next insert goes to the new table from here on.
EXCHANGE TABLES telegram_user_bot.events_log AND telegram_user_bot.events_log_unpartitioned;

-- Whatever was written to the old table while the copy ran. Everything else in
-- it is already here and collapses away on the sort key.
INSERT INTO telegram_user_bot.events_log
SELECT * FROM telegram_user_bot.events_log_unpartitioned;

DROP TABLE IF EXISTS telegram_user_bot.events_log_unpartitioned;

-- ---------------------------------------------------------------------------
-- 3. `events_daily_stat`, rebuilt unpartitioned.
--
--    One row per day / chat / topic / event: a month of it is a few thousand
--    rows, and it was cut into monthly pieces too. It is not copied but
--    recomputed from `events_log`, which is cheaper than reading the aggregate
--    states out and is exactly what the view below would have produced.
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS telegram_user_bot.events_daily_stat;

CREATE TABLE telegram_user_bot.events_daily_stat
(
    day         Date,
    chat_id     Int64,
    topic_id    Int32,
    event       LowCardinality(String),
    events      AggregateFunction(uniqExact, Tuple(Int64, String, DateTime)),
    senders     AggregateFunction(groupUniqArray, UInt64),
    media       AggregateFunction(groupUniqArrayIf, Tuple(Int64, UInt64), UInt8)
)
ENGINE = AggregatingMergeTree
ORDER BY (day, chat_id, topic_id, event);

-- ---------------------------------------------------------------------------
-- 4. The aggregates, back on the new tables. Definitions unchanged from 019.
-- ---------------------------------------------------------------------------

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_chat_stat
TO telegram_user_bot.events_chat_stat AS
SELECT
    chat_id,
    anyLastIfState(toString(chat_title), chat_title != '') AS last_title,
    uniqExactIfState((message_id, event, date_time), event = 'send') AS messages,
    uniqExactIfState((message_id, event, date_time), toUInt8((event = 'send') AND out)) AS outgoing,
    uniqExactIfState((message_id, event, date_time), (event = 'send') AND (reply_to != 0)) AS replies,
    uniqExactIfState((message_id, event, date_time), (event = 'send') AND (media_type != '')) AS media_messages,
    uniqExactIfState((message_id, event, date_time), event = 'edit') AS edits,
    uniqExactIfState((message_id, event, date_time), event = 'delete') AS deletes,
    groupUniqArrayIfState(user_id, (event = 'send') AND (user_id != 0)) AS participants,
    maxIfState(message_id, event = 'send') AS last_message_id,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM telegram_user_bot.events_log
WHERE (s3_key = '') AND NOT ephemeral
GROUP BY chat_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_user_stat
TO telegram_user_bot.events_user_stat AS
SELECT
    user_id,
    anyLastIfState(username, notEmpty(username)) AS username,
    anyLastIfState(first_name, first_name != '') AS first_name,
    anyLastIfState(second_name, second_name != '') AS second_name,
    groupUniqArrayState(chat_id) AS chats,
    uniqExactIfState((message_id, event, date_time), event = 'send') AS messages,
    uniqExactIfState((message_id, event, date_time), (event = 'send') AND (reply_to != 0)) AS replies,
    uniqExactIfState((message_id, event, date_time), (event = 'send') AND (media_type != '')) AS media_messages,
    uniqExactIfState((message_id, event, date_time), event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxState(date_time) AS last_seen
FROM telegram_user_bot.events_log
WHERE (s3_key = '') AND NOT ephemeral AND (event != 'delete') AND (user_id != 0)
GROUP BY user_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_daily_stat
TO telegram_user_bot.events_daily_stat AS
SELECT
    toDate(date_time) AS day,
    chat_id,
    topic_id,
    event,
    uniqExactState((message_id, event, date_time)) AS events,
    groupUniqArrayState(user_id) AS senders,
    groupUniqArrayIfState((message_id, size), size != 0) AS media
FROM telegram_user_bot.events_log
WHERE (s3_key = '') AND NOT ephemeral
GROUP BY day, chat_id, topic_id, event;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_edit_chain_stat
TO telegram_user_bot.events_edit_chain_stat AS
SELECT
    chat_id,
    message_id,
    uniqExactIfState((message_id, event, date_time), event IN ('send', 'edit')) AS versions,
    uniqExactIfState((message_id, event, date_time), event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxIfState(date_time, event = 'edit') AS last_edit,
    maxIfState(date_time, event = 'delete') AS deleted
FROM telegram_user_bot.events_log
WHERE (s3_key = '') AND NOT ephemeral
GROUP BY chat_id, message_id;

-- ---------------------------------------------------------------------------
-- 5. `events_daily_stat` is empty and its view only sees new inserts, so the
--    history is filled in once, by hand, with the view's own query.
-- ---------------------------------------------------------------------------

INSERT INTO telegram_user_bot.events_daily_stat
SELECT
    toDate(date_time) AS day,
    chat_id,
    topic_id,
    event,
    uniqExactState((message_id, event, date_time)) AS events,
    groupUniqArrayState(user_id) AS senders,
    groupUniqArrayIfState((message_id, size), size != 0) AS media
FROM telegram_user_bot.events_log
WHERE (s3_key = '') AND NOT ephemeral
GROUP BY day, chat_id, topic_id, event;
