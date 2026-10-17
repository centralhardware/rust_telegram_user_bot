-- The `work` account's own session tables, copied into the shared ones by 051.
-- Apply only once the bot built for 051 has started both accounts from the
-- shared tables: until then these are the only other copy of the work
-- account's DC keys.

DROP TABLE IF EXISTS telegram_user_bot.session_dc_home_work;
DROP TABLE IF EXISTS telegram_user_bot.session_dc_option_work;
DROP TABLE IF EXISTS telegram_user_bot.session_update_state_work;
DROP TABLE IF EXISTS telegram_user_bot.session_channel_state_work;
DROP TABLE IF EXISTS telegram_user_bot.peer_cache_work;
