-- lols_spam_factor is a percent 0-100; a byte holds it, rounded to a whole
-- percent. The few existing values (the column is a day old) are cast by
-- ClickHouse, which truncates rather than rounds.
--
-- Same Buffer dance as 048: the Buffer is a copy of the table's columns, so it
-- is dropped and created again around the ALTER.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    MODIFY COLUMN lols_spam_factor Nullable(UInt8) COMMENT 'Share of the messages from the sender that were spam, whole percent 0-100. NULL when the API omits it.';

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);
