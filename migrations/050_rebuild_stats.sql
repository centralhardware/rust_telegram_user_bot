-- Rebuild the four aggregates from the deduplicated log.
--
-- The materialized views count every insert into events_log, and since late
-- September the bot was inserting replayed updates: the same row twice, which
-- ReplacingMergeTree collapsed in events_log but the aggregates kept counting
-- (a few hundred extra sends a day). The bot now drops a row it has just
-- written (db/ch/recent.rs); this clears what was counted before that.
--
-- Each table is emptied and refilled from events_log FINAL with its own view's
-- SELECT. Rows flushed from the Buffer between a TRUNCATE and its INSERT are
-- counted by both; migrations run at startup, before any handler writes, so
-- that is at most what the previous process left in the Buffer.

TRUNCATE TABLE telegram_user_bot.events_daily_stat;
INSERT INTO telegram_user_bot.events_daily_stat
SELECT toDate(date_time) AS day, chat_id, topic_id, event,
       countState() AS events,
       groupUniqArrayState(user_id) AS senders,
       sumState(size) AS media_bytes
FROM telegram_user_bot.events_log FINAL
WHERE NOT ephemeral
GROUP BY day, chat_id, topic_id, event;

TRUNCATE TABLE telegram_user_bot.events_chat_stat;
INSERT INTO telegram_user_bot.events_chat_stat
SELECT chat_id,
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

TRUNCATE TABLE telegram_user_bot.events_user_stat;
INSERT INTO telegram_user_bot.events_user_stat
SELECT user_id,
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
WHERE (NOT ephemeral) AND (event NOT IN ('delete', 'file_uploaded')) AND (user_id != 0)
GROUP BY user_id;

TRUNCATE TABLE telegram_user_bot.events_edit_chain_stat;
INSERT INTO telegram_user_bot.events_edit_chain_stat
SELECT chat_id, message_id,
       countIfState(event IN ('send', 'edit')) AS versions,
       countIfState(event = 'edit') AS edits,
       minState(date_time) AS first_seen,
       maxIfState(date_time, event = 'edit') AS last_edit,
       maxIfState(date_time, event = 'delete') AS deleted
FROM telegram_user_bot.events_log FINAL
WHERE (NOT ephemeral) AND (event IN ('send', 'edit', 'delete'))
GROUP BY chat_id, message_id;
