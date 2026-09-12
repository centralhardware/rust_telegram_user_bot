-- `version` leaves `events_log`.
--
-- 019 made the table `ReplacingMergeTree(version)` because the archiver used to
-- rewrite a send row with its `sha256` / `s3_*` filled and a newer version, and
-- the two copies had to collapse onto one. Nothing rewrites a row any more: an
-- archived file has been a `file_uploaded` row of its own since 035, and every
-- row the bot writes carries version 0. The column is a DateTime that has been
-- zero for its whole life, and the version argument decides nothing.
--
-- Without it the engine keeps the last row inserted for a sort key, which is
-- what version 0 everywhere already meant: a redelivered update -- a reconnect
-- replaying its backlog -- is the same row on the same key, and it does not
-- matter which of the two copies survives.
--
-- The engine argument cannot be altered and the column cannot be dropped while
-- the engine names it, so the table is rebuilt beside itself and swapped in, the
-- way 029 did it. The bot keeps writing throughout: EXCHANGE is atomic, and the
-- rows that land in the old table between the copy and the swap are re-inserted
-- from it afterwards, where they collapse onto the copies already there.
--
-- The aggregates are dropped and recreated around the swap, because a
-- materialized view follows its source table by UUID and EXCHANGE swaps the
-- UUIDs: left in place they would keep reading the table on its way out. Their
-- storage tables are not touched and keep their history, and every counter is
-- over the row's own identity, so a row counted twice changes nothing.
--
-- The column list is the table as it stands after 038, minus `version`, codecs
-- and comments and all. `SELECT *` is not used on either side of the copy: the
-- two tables differ by exactly this column, which is the point.

-- ---------------------------------------------------------------------------
-- 1. Unhook the aggregates.
-- ---------------------------------------------------------------------------

DROP VIEW IF EXISTS telegram_user_bot.mv_events_chat_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_user_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_daily_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_events_edit_chain_stat;

-- ---------------------------------------------------------------------------
-- 2. `events_log`, rebuilt without the version.
-- ---------------------------------------------------------------------------

SET allow_suspicious_low_cardinality_types = 1;

CREATE TABLE IF NOT EXISTS telegram_user_bot.events_log_versionless
(
    `date_time` DateTime CODEC(Delta(4), ZSTD(9)),
    `event` LowCardinality(String),
    `chat_id` Int64,
    `chat_title` LowCardinality(String),
    `message_id` Int64 CODEC(Delta(8), ZSTD(9)),
    `message` String COMMENT 'What the sender typed, byte for byte -- or, when there is no text, the description of the media or the service action the message carries. The formatting over it is in `entities` and the buttons under it in `keyboard`. Rows written before migration 037 hold the rendered text with the buttons glued underneath instead.' CODEC(ZSTD(9)),
    `entities` Array(Tuple(LowCardinality(String), UInt32, UInt32, String)) COMMENT 'The formatting over `message`, one tuple per entity: (1) the type name the Bot API gives it, (2) where it starts and (3) how far it runs, both in UTF-16 code units, and (4) the one thing it carries besides -- the target of a link, the account of a mention, the language of a code block. `render_message_html` draws it back onto the text. Empty on every row written before migration 037, whose `message` holds the rendering instead.' CODEC(ZSTD(9)),
    `keyboard` Array(Tuple(UInt8, String, String, String)) COMMENT 'The inline keyboard under the message, one tuple per button: (1) the row it sits in, (2) its label, (3) what it does and (4) where it leads. `render_keyboard_html` draws it. Empty on every row written before migration 037, which glued the buttons onto the end of `message` instead.' CODEC(ZSTD(9)),
    `user_id` UInt64 CODEC(ZSTD(9)),
    `username` Array(String) CODEC(ZSTD(9)),
    `first_name` String CODEC(ZSTD(9)),
    `second_name` String CODEC(ZSTD(9)),
    `community_tag` LowCardinality(String),
    `community_id` Int64,
    `chat_usernames` Array(LowCardinality(String)),
    `reply_to` UInt64 CODEC(ZSTD(9)),
    `reply_to_user_id` UInt64 CODEC(ZSTD(9)),
    `reply_to_chat_id` Int64 DEFAULT 0,
    `quote_text` String DEFAULT '' CODEC(ZSTD(9)),
    `comment_to` UInt64 DEFAULT 0 CODEC(ZSTD(9)),
    `topic_id` Int32,
    `topic_name` LowCardinality(String),
    `fwd_from_user_id` UInt64,
    `fwd_from_chat_id` Int64,
    `fwd_from_msg_id` Int64,
    `fwd_from_name` String CODEC(ZSTD(9)),
    `fwd_date` DateTime,
    `action` LowCardinality(String),
    `service_message_id` Int64 DEFAULT 0,
    `grouped_id` UInt64,
    `reactions` Map(String, UInt32),
    `ephemeral` Bool,
    `receiver_id` UInt64,
    `reply_to_ephemeral` Bool,
    `welcome` Bool,
    `out` Bool,
    `raw` String CODEC(ZSTD(9)),
    `via_bot_id` UInt64,
    `post_author` String CODEC(ZSTD(9)),
    `guest_from_id` Int64,
    `pinned` Bool,
    `silent` Bool,
    `noforwards` Bool,
    `ttl_period` UInt32,
    `diff` String COMMENT 'Edits only: a unified diff, counted in words, against the text this edit replaced. That text is not stored -- `edit_prev_text` puts it back together out of this and `message`, and `edit_diff_html` renders the marked-up version the boards print.' CODEC(ZSTD(9)),
    `media_type` LowCardinality(String),
    `file_name` String CODEC(ZSTD(9)),
    `mime_type` LowCardinality(String),
    `size` UInt64,
    `duration` UInt32,
    `width` UInt32,
    `height` UInt32,
    `lat` Float64,
    `lon` Float64,
    `poll_question` String CODEC(ZSTD(9)),
    `poll_options` Array(String) CODEC(ZSTD(9)),
    `sha256` String,
    `s3_bucket` LowCardinality(String),
    `s3_key` String,
    `poll_id` Int64 CODEC(ZSTD(9)),
    `poll_results` Map(String, UInt32),
    `poll_total_voters` UInt32,
    `views` UInt32,
    `forwards` UInt32
)
ENGINE = ReplacingMergeTree
ORDER BY (chat_id, ephemeral, message_id, event, date_time);

INSERT INTO telegram_user_bot.events_log_versionless
(
    date_time, event, chat_id, chat_title, message_id, message, entities, keyboard,
    user_id, username, first_name, second_name, community_tag, community_id,
    chat_usernames, reply_to, reply_to_user_id, reply_to_chat_id, quote_text,
    comment_to, topic_id, topic_name, fwd_from_user_id, fwd_from_chat_id,
    fwd_from_msg_id, fwd_from_name, fwd_date, action, service_message_id,
    grouped_id, reactions, ephemeral, receiver_id, reply_to_ephemeral, welcome,
    out, raw, via_bot_id, post_author, guest_from_id, pinned, silent, noforwards,
    ttl_period, diff, media_type, file_name, mime_type, size, duration, width,
    height, lat, lon, poll_question, poll_options, sha256, s3_bucket, s3_key,
    poll_id, poll_results, poll_total_voters, views, forwards
)
SELECT
    date_time, event, chat_id, chat_title, message_id, message, entities, keyboard,
    user_id, username, first_name, second_name, community_tag, community_id,
    chat_usernames, reply_to, reply_to_user_id, reply_to_chat_id, quote_text,
    comment_to, topic_id, topic_name, fwd_from_user_id, fwd_from_chat_id,
    fwd_from_msg_id, fwd_from_name, fwd_date, action, service_message_id,
    grouped_id, reactions, ephemeral, receiver_id, reply_to_ephemeral, welcome,
    out, raw, via_bot_id, post_author, guest_from_id, pinned, silent, noforwards,
    ttl_period, diff, media_type, file_name, mime_type, size, duration, width,
    height, lat, lon, poll_question, poll_options, sha256, s3_bucket, s3_key,
    poll_id, poll_results, poll_total_voters, views, forwards
FROM telegram_user_bot.events_log;

-- The bot's next insert goes to the new table from here on.
EXCHANGE TABLES telegram_user_bot.events_log AND telegram_user_bot.events_log_versionless;

-- Whatever was written to the old table while the copy ran. Everything else in
-- it is already here and collapses away on the sort key.
INSERT INTO telegram_user_bot.events_log
(
    date_time, event, chat_id, chat_title, message_id, message, entities, keyboard,
    user_id, username, first_name, second_name, community_tag, community_id,
    chat_usernames, reply_to, reply_to_user_id, reply_to_chat_id, quote_text,
    comment_to, topic_id, topic_name, fwd_from_user_id, fwd_from_chat_id,
    fwd_from_msg_id, fwd_from_name, fwd_date, action, service_message_id,
    grouped_id, reactions, ephemeral, receiver_id, reply_to_ephemeral, welcome,
    out, raw, via_bot_id, post_author, guest_from_id, pinned, silent, noforwards,
    ttl_period, diff, media_type, file_name, mime_type, size, duration, width,
    height, lat, lon, poll_question, poll_options, sha256, s3_bucket, s3_key,
    poll_id, poll_results, poll_total_voters, views, forwards
)
SELECT
    date_time, event, chat_id, chat_title, message_id, message, entities, keyboard,
    user_id, username, first_name, second_name, community_tag, community_id,
    chat_usernames, reply_to, reply_to_user_id, reply_to_chat_id, quote_text,
    comment_to, topic_id, topic_name, fwd_from_user_id, fwd_from_chat_id,
    fwd_from_msg_id, fwd_from_name, fwd_date, action, service_message_id,
    grouped_id, reactions, ephemeral, receiver_id, reply_to_ephemeral, welcome,
    out, raw, via_bot_id, post_author, guest_from_id, pinned, silent, noforwards,
    ttl_period, diff, media_type, file_name, mime_type, size, duration, width,
    height, lat, lon, poll_question, poll_options, sha256, s3_bucket, s3_key,
    poll_id, poll_results, poll_total_voters, views, forwards
FROM telegram_user_bot.events_log_versionless;

DROP TABLE IF EXISTS telegram_user_bot.events_log_versionless;

-- ---------------------------------------------------------------------------
-- 3. The aggregates, back on the new table. Definitions unchanged: these are
--    the ones the server was running, read back off `system.tables`.
-- ---------------------------------------------------------------------------

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_chat_stat TO telegram_user_bot.events_chat_stat AS
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
FROM telegram_user_bot.events_log
WHERE NOT ephemeral
GROUP BY chat_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_user_stat TO telegram_user_bot.events_user_stat AS
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
WHERE (NOT ephemeral) AND (event NOT IN ('delete', 'file_uploaded')) AND (user_id != 0)
GROUP BY user_id;

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_daily_stat TO telegram_user_bot.events_daily_stat AS
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

CREATE MATERIALIZED VIEW IF NOT EXISTS telegram_user_bot.mv_events_edit_chain_stat TO telegram_user_bot.events_edit_chain_stat AS
SELECT
    chat_id,
    message_id,
    countIfState(event IN ('send', 'edit')) AS versions,
    countIfState(event = 'edit') AS edits,
    minState(date_time) AS first_seen,
    maxIfState(date_time, event = 'edit') AS last_edit,
    maxIfState(date_time, event = 'delete') AS deleted
FROM telegram_user_bot.events_log
WHERE (NOT ephemeral) AND (event IN ('send', 'edit', 'delete'))
GROUP BY chat_id, message_id;
