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
-- The old tables are kept as `*_legacy` and their names are given to views over
-- `events_log`, so the Grafana boards and every existing query keep working. Nothing
-- is backfilled: the history stays in the `*_legacy` tables and the views show what
-- is logged from here on. Run this with the bot stopped — the moment the old names
-- become views they stop accepting inserts.

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
    reply_to         UInt64,
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
    client_id        UInt64,
    -- Ingest time: the newest write of a key wins.
    version          DateTime
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(date_time)
ORDER BY (chat_id, message_id, event, date_time);


-- ---------------------------------------------------------------------------
-- The old names, kept as views over the new table.
-- ---------------------------------------------------------------------------

-- The aggregates first: they read the tables that are about to be renamed, and
-- `mv_my_messages_to_chats_log` has nothing left to do — outgoing messages are
-- written straight into `events_log`.
DROP VIEW IF EXISTS mv_my_messages_to_chats_log;
DROP VIEW IF EXISTS mv_chat_stat;
DROP VIEW IF EXISTS mv_user_stat;
DROP VIEW IF EXISTS mv_message_stats;
DROP VIEW IF EXISTS mv_edited_chain_stats;

RENAME TABLE
    chats_log             TO chats_log_legacy,
    telegram_messages_new TO telegram_messages_new_legacy,
    edited_log            TO edited_log_legacy,
    deleted_log           TO deleted_log_legacy,
    media_log             TO media_log_legacy;

CREATE VIEW chats_log AS
SELECT date_time, message, chat_title, chat_id, username, first_name, second_name,
       user_id, community_tag, message_id, chat_usernames, reply_to, client_id
FROM events_log
WHERE event = 'send';

CREATE VIEW telegram_messages_new AS
SELECT date_time, message, chat_title AS title, chat_id AS id,
       CAST([] AS Array(LowCardinality(String))) AS admins2,
       chat_usernames AS usernames, toUInt64(message_id) AS message_id,
       reply_to, raw, client_id
FROM events_log
WHERE event = 'send' AND out;

-- `original_message` is no longer stored: for an edit it is simply the text the
-- message had one event earlier, so the view reads it back off the preceding send
-- or edit row instead of keeping a second copy of it.
CREATE VIEW edited_log AS
SELECT date_time, chat_id, message_id, original_message, message, diff,
       toInt64(user_id) AS user_id, client_id
FROM
(
    SELECT
        date_time, chat_id, message_id, message, diff, user_id, client_id, event,
        lagInFrame(message) OVER
        (
            PARTITION BY chat_id, message_id ORDER BY date_time ASC
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS original_message
    FROM events_log
    WHERE event IN ('send', 'edit')
)
WHERE event = 'edit';

CREATE VIEW deleted_log AS
SELECT date_time, chat_id, message_id, client_id
FROM events_log
WHERE event = 'delete';

CREATE VIEW media_log AS
SELECT date_time, chat_id, chat_title, message_id, user_id, media_type, file_name,
       mime_type, size, sha256, s3_bucket, s3_key
FROM events_log
WHERE event = 'send' AND s3_key != '';


-- ---------------------------------------------------------------------------
-- The aggregates again, on the new source. Their target tables are the existing
-- ones and are left untouched, so the counters carry on from where the old
-- materialized views left them.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS chat_stat
(
    client_id       UInt64,
    chat_id         Int64,
    last_title      String,
    msg_count       AggregateFunction(count),
    reply_msg_count AggregateFunction(sum, UInt8),
    participants    AggregateFunction(groupUniqArray, UInt64),
    last_message_id AggregateFunction(max, Int64)
)
ENGINE = AggregatingMergeTree
ORDER BY (client_id, chat_id);

CREATE MATERIALIZED VIEW mv_chat_stat TO chat_stat AS
SELECT
    client_id,
    chat_id,
    anyLast(chat_title) AS last_title,
    countState() AS msg_count,
    sumState(if(reply_to != 0, 1, 0)) AS reply_msg_count,
    groupUniqArrayState(user_id) AS participants,
    maxState(message_id) AS last_message_id
FROM events_log
-- The archiver rewrites a send row once its file is in S3; that rewrite must not
-- be counted a second time, and the row it replaces was already counted.
WHERE event = 'send' AND s3_key = ''
GROUP BY client_id, chat_id;


CREATE TABLE IF NOT EXISTS user_stat
(
    client_id       UInt64,
    user_id         UInt64,
    username        Array(String),
    first_name      String,
    second_name     String,
    chats           AggregateFunction(groupUniqArray, Int64),
    msg_count       AggregateFunction(count),
    reply_msg_count AggregateFunction(sum, UInt8)
)
ENGINE = AggregatingMergeTree
ORDER BY (client_id, user_id);

CREATE MATERIALIZED VIEW mv_user_stat TO user_stat AS
SELECT
    client_id,
    user_id,
    anyLast(username) AS username,
    anyLast(first_name) AS first_name,
    anyLast(second_name) AS second_name,
    groupUniqArrayState(chat_id) AS chats,
    countState() AS msg_count,
    sumState(if(reply_to != 0, 1, 0)) AS reply_msg_count
FROM events_log
WHERE event = 'send' AND s3_key = ''
GROUP BY client_id, user_id;


CREATE TABLE IF NOT EXISTS message_stats
(
    id         Int64,
    client_id  UInt64,
    last_title AggregateFunction(anyLast, String),
    cnt_state  AggregateFunction(count)
)
ENGINE = AggregatingMergeTree
ORDER BY (id, client_id);

CREATE MATERIALIZED VIEW mv_message_stats TO message_stats AS
SELECT
    chat_id AS id,
    client_id,
    anyLastState(chat_title) AS last_title,
    countState() AS cnt_state
FROM events_log
WHERE event = 'send' AND out AND s3_key = ''
GROUP BY id, client_id;


-- The edit-chain counters keep their existing target table, so there is nothing to
-- backfill: only the source changes.
CREATE MATERIALIZED VIEW mv_edited_chain_stats TO edited_chain_stats AS
SELECT
    chat_id,
    message_id,
    countState() AS versions_state,
    minState(date_time) AS first_time_state,
    maxState(date_time) AS last_time_state
FROM events_log
WHERE event = 'edit'
GROUP BY chat_id, message_id;
