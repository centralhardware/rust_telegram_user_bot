-- Who a message @mentions, kept at insert time so a query (who mentioned me,
-- who I mentioned, the who-talks-to-whom graph) reads two arrays instead of
-- slicing `message` by `entities` on every row.
--
-- `mentions` is the usernames typed as `@name`, lowercased and without the
-- `@`. `mentioned_user_ids` is the accounts linked by a text mention (an
-- account with no username, linked by name). Each listed once. Filled on send
-- and edit rows by the bot (`entities::mentions`).
--
-- Added the way 055 added file_dc_id: the Buffer is a copy of the table's
-- columns, so it is dropped and created again around the ALTER.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS mentions Array(String) COMMENT 'Usernames the message @mentions, lowercased, without the @.' AFTER reply_to_user_id,
    ADD COLUMN IF NOT EXISTS mentioned_user_ids Array(UInt64) COMMENT 'User ids the message links by text mention.' AFTER mentions;

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);

-- Backfill the rows logged before this. ClickHouse slices by code points while
-- Telegram counts UTF-16 units, so a mention after an emoji or another astral
-- character can come out shifted on these old rows; new rows are sliced right
-- by the bot. A heavy mutation: run it on its own and watch system.mutations.
ALTER TABLE telegram_user_bot.events_log
    UPDATE
        mentions = arrayDistinct(arrayFilter(x -> x != '', arrayMap(
            e -> lower(trimLeft(substringUTF8(message, e.2 + 1, e.3), '@')),
            arrayFilter(e -> e.1 = 'mention', entities)))),
        mentioned_user_ids = arrayDistinct(arrayMap(
            e -> toUInt64OrZero(e.4),
            arrayFilter(e -> e.1 = 'text_mention' AND toUInt64OrZero(e.4) != 0, entities)))
    WHERE event IN ('send', 'edit')
      AND arrayExists(e -> e.1 IN ('mention', 'text_mention'), entities);
