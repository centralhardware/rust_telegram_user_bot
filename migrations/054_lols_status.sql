-- Whether a user the bot has seen is on the lols.bot spam banlist
-- (https://lols.bot). Written by `src/lols.rs`, one row per check.
--
-- The check runs in two steps. The hourly bulk dump is tried first: it is one
-- download for every user and needs no request per id. Only a user missing from
-- the dump is asked about through the API, which also catches a ban newer than
-- the dump. `source` says which step answered.
--
-- A user found banned is not checked again. A user found clean is matched
-- against every new dump, and asked about through the API again after a week.
--
-- Read the latest state with argMax over `checked_at`, or with FINAL.
CREATE TABLE IF NOT EXISTS telegram_user_bot.lols_status
(
    `user_id` Int64 COMMENT 'Bare Telegram user id, as peer_names.peer_id holds it.',
    `banned` Bool COMMENT 'On the lols.bot banlist.',
    `source` LowCardinality(String) COMMENT '`dump` (banlist.txt) or `api` (api.lols.bot/account).',
    `banned_at` Nullable(DateTime) COMMENT 'When lols.bot banned the user. Only the API reports it.',
    `offenses` Nullable(UInt32) COMMENT 'Spam messages lols.bot recorded. API only.',
    `spam_factor` Nullable(Float32) COMMENT 'Share of the user''s messages that were spam, in percent. API only.',
    `scammer` Bool DEFAULT false COMMENT 'A verified scammer. API only.',
    `checked_at` DateTime
)
ENGINE = ReplacingMergeTree(checked_at)
ORDER BY user_id;
