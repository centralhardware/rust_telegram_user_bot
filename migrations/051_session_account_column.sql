-- One set of session tables for every account, instead of a `_<name>` copy of
-- each per account.
--
-- `account` is the account's name, '' for the first one -- the same string the
-- tables used to be suffixed with, minus the underscore. Every row already in
-- the unsuffixed tables is the first account's, and '' is what a String column
-- added without a DEFAULT reads as, so nothing there is backfilled.
--
-- It goes at the end of each sorting key: appending a column added in the same
-- ALTER is the one ORDER BY change ClickHouse allows without a rebuild (049).
-- With it in the key, ReplacingMergeTree never collapses one account's DC keys,
-- update position or access hash into another's.
--
-- Stop the bot before applying: the old one still writes to the `_work`
-- tables, which are copied here. Dropping them is left to 052, once the new
-- bot is confirmed working.

ALTER TABLE telegram_user_bot.session_dc_home
    ADD COLUMN IF NOT EXISTS account LowCardinality(String),
    MODIFY ORDER BY (key, account);
ALTER TABLE telegram_user_bot.session_dc_option
    ADD COLUMN IF NOT EXISTS account LowCardinality(String),
    MODIFY ORDER BY (dc_id, account);
ALTER TABLE telegram_user_bot.session_update_state
    ADD COLUMN IF NOT EXISTS account LowCardinality(String),
    MODIFY ORDER BY (key, account);
ALTER TABLE telegram_user_bot.session_channel_state
    ADD COLUMN IF NOT EXISTS account LowCardinality(String),
    MODIFY ORDER BY (peer_id, account);
ALTER TABLE telegram_user_bot.peer_cache
    ADD COLUMN IF NOT EXISTS account LowCardinality(String),
    MODIFY ORDER BY (peer_id, account);

-- The Buffer is dropped (which flushes it) and made again with the new column.
DROP TABLE IF EXISTS telegram_user_bot.peer_cache_buffer;
CREATE TABLE IF NOT EXISTS telegram_user_bot.peer_cache_buffer AS telegram_user_bot.peer_cache
ENGINE = Buffer('telegram_user_bot', peer_cache, 1, 10, 60, 100, 10000, 65536, 4194304);

-- The `work` account's rows move in under its name. Its Buffer goes first so
-- its last rows are flushed into peer_cache_work before the copy.
DROP TABLE IF EXISTS telegram_user_bot.peer_cache_buffer_work;

INSERT INTO telegram_user_bot.session_dc_home (key, dc_id, updated_at, account)
    SELECT key, dc_id, updated_at, 'work' FROM telegram_user_bot.session_dc_home_work;
INSERT INTO telegram_user_bot.session_dc_option (dc_id, ipv4, ipv6, auth_key, updated_at, account)
    SELECT dc_id, ipv4, ipv6, auth_key, updated_at, 'work' FROM telegram_user_bot.session_dc_option_work;
INSERT INTO telegram_user_bot.session_update_state (key, pts, qts, date, seq, updated_at, account)
    SELECT key, pts, qts, date, seq, updated_at, 'work' FROM telegram_user_bot.session_update_state_work;
INSERT INTO telegram_user_bot.session_channel_state (peer_id, pts, updated_at, account)
    SELECT peer_id, pts, updated_at, 'work' FROM telegram_user_bot.session_channel_state_work;
INSERT INTO telegram_user_bot.peer_cache (peer_id, hash, subtype, updated_at, account)
    SELECT peer_id, hash, subtype, updated_at, 'work' FROM telegram_user_bot.peer_cache_work;
