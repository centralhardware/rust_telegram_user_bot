-- What lols.bot (https://lols.bot) said about the sender of an incoming
-- message when it was logged. Set by `src/lols.rs`. lols.bot folds combot
-- (CAS) bans into its own `banned`, and does not say which list a ban is from.
--
-- `lols_source` is '' when nothing was checked (outgoing messages, backfills,
-- channel posts, an API failure or timeout), `dump` when the hourly banlist
-- answered, `api` when api.lols.bot/account did. Only `api` carries the details;
-- a cached API answer is written as `api` too.
--
-- Added the way 048 added sender_chat_id: the Buffer is a copy of the table's
-- columns, so it is dropped and created again around the ALTER.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS lols_banned Bool DEFAULT false AFTER sender_chat_id,
    ADD COLUMN IF NOT EXISTS lols_source LowCardinality(String) AFTER lols_banned,
    ADD COLUMN IF NOT EXISTS lols_when Nullable(DateTime) AFTER lols_source,
    ADD COLUMN IF NOT EXISTS lols_offenses Nullable(UInt32) AFTER lols_when,
    ADD COLUMN IF NOT EXISTS lols_spam_factor Nullable(Float32) AFTER lols_offenses,
    ADD COLUMN IF NOT EXISTS lols_scammer Bool DEFAULT false AFTER lols_spam_factor,
    ADD COLUMN IF NOT EXISTS lols_scamrsalert Nullable(UInt32) AFTER lols_scammer;

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);
