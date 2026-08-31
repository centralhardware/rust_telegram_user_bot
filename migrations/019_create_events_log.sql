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
-- This migration creates the table and nothing else. The old tables keep their
-- names and their history and are not touched, not renamed and not backfilled;
-- they simply stop being written. Their materialized views are left attached to
-- them too, so the aggregates (`chat_stat`, `user_stat`, `message_stats`,
-- `edited_chain_stats`) keep what they hold and stop advancing — re-pointing them
-- at `events_log` is a separate step, as is moving the Grafana boards over.

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
    client_id        UInt64,
    -- Ingest time: the newest write of a key wins.
    version          DateTime
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(date_time)
ORDER BY (chat_id, message_id, event, date_time);
