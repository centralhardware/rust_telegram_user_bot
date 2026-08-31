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
-- `events_log`, so the Grafana boards and every existing query keep working.

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
    -- Edits only: the text the edit replaced, and the unified diff between the two.
    original_message String,
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
-- Backfill. Run before the views are created and before the new build is
-- deployed, with the bot stopped: the old tables must not be written while they
-- are being read, and the moment they become views they stop accepting inserts.
-- ---------------------------------------------------------------------------

-- Received messages. Rows where the sender is this account are the ones
-- `mv_my_messages_to_chats_log` copied over from `telegram_messages_new`; they are
-- inserted below from that table instead, which is where their `raw` lives.
INSERT INTO events_log
    (date_time, event, chat_id, chat_title, message_id, message, user_id, username,
     first_name, second_name, community_tag, chat_usernames, reply_to, out, client_id, version)
SELECT
    date_time, 'send', chat_id, chat_title, message_id, message, user_id, username,
    first_name, second_name, community_tag, chat_usernames, reply_to, false, client_id, date_time
FROM chats_log
WHERE user_id != client_id;

-- Sent by this account.
INSERT INTO events_log
    (date_time, event, chat_id, chat_title, message_id, message, user_id,
     chat_usernames, reply_to, out, raw, client_id, version)
SELECT
    date_time, 'send', id, title, toInt64(message_id), message, client_id,
    usernames, reply_to, true, raw, client_id, date_time
FROM telegram_messages_new;

-- Edits.
INSERT INTO events_log
    (date_time, event, chat_id, message_id, message, original_message, diff, user_id, client_id, version)
SELECT
    date_time, 'edit', chat_id, message_id, message, original_message, diff,
    toUInt64(user_id), client_id, date_time
FROM edited_log;

-- Deletions. `message` stays empty for these: what the message said is only
-- recoverable through the send row, which is what `delete_log_hr` joins in. Rows
-- written from here on carry the text themselves.
INSERT INTO events_log
    (date_time, event, chat_id, message_id, client_id, version)
SELECT date_time, 'delete', chat_id, message_id, client_id, date_time
FROM deleted_log;

-- Archived media, folded onto the send row it belongs to — the same row again with
-- the file columns filled and a newer version.
INSERT INTO events_log
    (date_time, event, chat_id, chat_title, message_id, message, user_id, username,
     first_name, second_name, community_tag, chat_usernames, reply_to, out, raw,
     original_message, diff, media_type, file_name, mime_type, size, sha256,
     s3_bucket, s3_key, client_id, version)
SELECT
    e.date_time, e.event, e.chat_id, e.chat_title, e.message_id, e.message, e.user_id, e.username,
    e.first_name, e.second_name, e.community_tag, e.chat_usernames, e.reply_to, e.out, e.raw,
    e.original_message, e.diff, m.media_type, m.file_name, m.mime_type, m.size, m.sha256,
    m.s3_bucket, m.s3_key, e.client_id, now()
FROM events_log AS e
INNER JOIN media_log AS m USING (chat_id, message_id)
WHERE e.event = 'send';


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

CREATE VIEW edited_log AS
SELECT date_time, chat_id, message_id, original_message, message, diff,
       toInt64(user_id) AS user_id, client_id
FROM events_log
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
-- The aggregates again, on the new source. Their targets are explicit tables now,
-- so the history can be inserted after the view is attached — a POPULATE would
-- have missed whatever arrived while it ran.
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

INSERT INTO chat_stat
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

INSERT INTO user_stat
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

INSERT INTO message_stats
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
