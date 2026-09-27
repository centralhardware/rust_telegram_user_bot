-- A second Telegram account, logging into the same tables.
--
-- In a channel or supergroup a message has one id for every member, so the
-- accounts share its rows, and only one of them writes each event (the lock is
-- in the bot: `state::claims`). Private chats and basic groups are different:
-- each account numbers its own messages there, so account A's message 5 with
-- user C and account B's message 5 with user C are two messages with the same
-- (chat_id, message_id). `account_id` tells them apart, and is in the key so
-- ReplacingMergeTree never collapses one into the other.
--
-- 0 means the first account, and every channel row whichever account wrote it.
-- That is what every row written before this migration already is, so nothing
-- is backfilled. A later account writes its own user id on its private-chat and
-- basic-group rows.
--
-- A column added in the same ALTER may be appended to the sorting key, which
-- is the one ORDER BY change ClickHouse allows without a rebuild -- as long as
-- it has no DEFAULT expression (Code 36 otherwise). UInt64 is 0 when unset
-- anyway, which is the value every existing row needs. The Buffer is then
-- dropped (which flushes it) and made again from the new table.
--
-- The ALTER comes first, unlike 048: if it fails, the Buffer the running bot
-- writes to is still there and nothing is lost. Until the Buffer is remade it
-- just lacks the new column, and its rows flush down with 0 in it -- right for
-- the first account, the only one an older bot runs.

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS account_id UInt64,
    MODIFY ORDER BY (chat_id, ephemeral, message_id, event, date_time, account_id);

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

CREATE TABLE IF NOT EXISTS telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);

-- The accounts after the first, as `setup <name>` records them. The running bot
-- reads this every minute and starts any account it is not running yet. Each
-- one's grammers session is in its own copy of the session tables, named with
-- `_<name>` on the end; the first account keeps the tables without a suffix.
CREATE TABLE IF NOT EXISTS telegram_user_bot.accounts
(
    name     String,
    user_id  UInt64,
    added_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(added_at)
ORDER BY name;
