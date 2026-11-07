-- Whether the sender of an incoming message was on the lols.bot spam banlist
-- (https://lols.bot) when the message was logged. Set by `src/lols.rs`.
-- false for outgoing messages, backfills, channel posts, and whenever the
-- check could not answer in time.
--
-- Added the way 048 added sender_chat_id: the Buffer is a copy of the table's
-- columns, so it is dropped and created again around the ALTER.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS sender_spam Bool DEFAULT false AFTER sender_chat_id;

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);
