-- The pre-`events_log` tables move to a database of their own.
--
-- Migration 019 replaced five tables with one: `chats_log` (received),
-- `telegram_messages_new` (sent by this account), `edited_log`, `deleted_log`
-- and `media_log` are all `events_log` now, and `ephemeral_log` went the same
-- way a day later. Nothing writes to any of them any more -- the bot's only
-- message writer is EVENTS_BUF -- and no dashboard queries them: they are
-- history, kept because they hold everything before 2026-08-31.
--
-- History that nothing reads still has to be read past on every `SHOW TABLES`,
-- so it moves out of `telegram_user_bot` and into `telegram_user_bot_legacy`.
-- The live database is left holding only what the bot actually uses.
--
-- Only the data moves. The views and materialized views that sat on top of
-- these tables are dropped, not carried over: the aggregates they maintained
-- are derived from rows that are all still here, and nothing will ever trigger
-- them again with their sources moved. Their definitions stay in git (see
-- `migrations/legacy/`) should any of it ever be wanted back.
--
-- Both databases are Atomic, so the RENAMEs are metadata operations: not one of
-- the 1.1 GiB is copied or rewritten.

CREATE DATABASE IF NOT EXISTS telegram_user_bot_legacy;

-- 1. Everything that reads the tables below, dropped first so nothing is left
--    pointing at a table that has moved out from under it. `mv_chat_stat`,
--    `mv_user_stat` and `mv_message_stats` own their storage (an implicit
--    `.inner_id.<uuid>` table), which goes with them.
DROP VIEW IF EXISTS telegram_user_bot.edit_log_hr;
DROP VIEW IF EXISTS telegram_user_bot.delete_log_hr;
DROP VIEW IF EXISTS telegram_user_bot.v_edited_chain_stats;
DROP VIEW IF EXISTS telegram_user_bot.mv_chat_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_user_stat;
DROP VIEW IF EXISTS telegram_user_bot.mv_message_stats;
DROP VIEW IF EXISTS telegram_user_bot.mv_edited_chain_stats;
DROP VIEW IF EXISTS telegram_user_bot.mv_my_messages_to_chats_log;

-- 2. The history itself.
RENAME TABLE
    telegram_user_bot.chats_log             TO telegram_user_bot_legacy.chats_log,
    telegram_user_bot.telegram_messages_new TO telegram_user_bot_legacy.telegram_messages_new,
    telegram_user_bot.edited_log            TO telegram_user_bot_legacy.edited_log,
    telegram_user_bot.deleted_log           TO telegram_user_bot_legacy.deleted_log,
    telegram_user_bot.media_log             TO telegram_user_bot_legacy.media_log,
    telegram_user_bot.ephemeral_log         TO telegram_user_bot_legacy.ephemeral_log,
    telegram_user_bot.edited_chain_stats    TO telegram_user_bot_legacy.edited_chain_stats;
