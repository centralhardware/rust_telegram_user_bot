-- What lols.bot (https://lols.bot) said about the sender of an incoming
-- message when it was logged. Set by `src/lols.rs`. lols.bot folds combot
-- (CAS) bans into its own `banned`, and does not say which list a ban is from.
--
-- The details (`lols_offenses` and on) come only from the API; a sender
-- found in the hourly banlist dump gets the ban alone.
--
-- Added the way 048 added sender_chat_id: the Buffer is a copy of the table's
-- columns, so it is dropped and created again around the ALTER.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS lols_banned Bool DEFAULT false COMMENT 'Sender is on the lols.bot banlist (combot/CAS bans included).' AFTER sender_chat_id,
    ADD COLUMN IF NOT EXISTS lols_offenses Nullable(UInt32) COMMENT 'Spam messages lols.bot recorded from the sender. NULL when the API omits it.' AFTER lols_banned,
    ADD COLUMN IF NOT EXISTS lols_spam_factor Nullable(Float32) COMMENT 'Share of the sender\'s messages that were spam, percent 0-100. NULL when the API omits it.' AFTER lols_offenses,
    ADD COLUMN IF NOT EXISTS lols_scammer Bool DEFAULT false COMMENT 'lols.bot lists the sender as a verified scammer.' AFTER lols_spam_factor,
    ADD COLUMN IF NOT EXISTS lols_scamrsalert Nullable(UInt32) COMMENT 'Post id about the sender in t.me/scamrsalert. NULL when there is none.' AFTER lols_scammer;

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);
