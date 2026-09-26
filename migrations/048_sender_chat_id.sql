-- Who a message was sent as, when no user sent it.
--
-- A message sent "as" a channel -- a member posting through their channel, an
-- anonymous admin, a channel's own post in its discussion group -- carries a
-- channel in `from_id` and no user at all. `user_id` is 0 for those rows and
-- the channel was only ever in `raw`. `sender_chat_id` keeps it as a column,
-- a Bot API dialog id like the Bot API's own `sender_chat`; 0 when a user sent
-- the message.
--
-- The Buffer has to be dropped and recreated around the ALTER: its structure
-- must match `events_log`, and dropping it flushes whatever it holds. The
-- runner applies this at startup, before the bot writes anything.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS sender_chat_id Int64 AFTER guest_from_id;

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);

-- Backfill from `raw`. A channel peer's dialog id is -(10^12 + channel_id).
ALTER TABLE telegram_user_bot.events_log
    UPDATE sender_chat_id = -(1000000000000 + JSONExtractInt(raw, 'Message', 'from_id', 'Channel', 'channel_id'))
    WHERE user_id = 0 AND JSONHas(raw, 'Message', 'from_id', 'Channel');
